import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import {sources, matrix, dependencyFingerprint, expectedTiming} from './observations.mts';

assert.equal(process.version, 'v22.22.0');
const root = process.env.PDRS_BASELINE;
assert.ok(root, 'set PDRS_BASELINE');
const { fetch, setGlobalDispatcher, getGlobalDispatcher } = await import(pathToFileURL(`${root}/node_modules/undici/index.js`).href);
const { networkFetch } = await import(pathToFileURL(`${root}/src/network/fetch.ts`).href);
const { getURLMarkdownContent } = await import(pathToFileURL(`${root}/src/tool/builtin/web/urlFetcher.ts`).href);
const realTimer = globalThis.setTimeout, realClear = globalThis.clearTimeout, realRandom = Math.random;
const oldDispatcher = getGlobalDispatcher();
for (const source of sources) console.log(JSON.stringify({source, sha256: createHash('sha256').update(readFileSync(`${root}/${source}`)).digest('hex'), node: process.version}));
console.log(JSON.stringify(dependencyFingerprint(root)));
const specs = matrix();
try {
  Math.random = () => 0;
  for (const spec of specs) {
    const {path, phase, event, order} = spec;
    const id = `${path}/${phase}/${event}/${order}`;
    let captured = false;
    for (let attempt = 0; attempt < 10; attempt++) {
      const parent = new AbortController();
      const handles = new Map<any, any>();
      const timers: any[] = [], owned: any[] = [], timeline: string[] = [], requests: string[] = [];
      const retryPhase = phase.startsWith('retry-');
      const bodyPhase = phase === 'body' || phase === 'text-body';
      const operationMs = event === 'idle' ? 40 : 20;
      const parentMs = operationMs + (order === 'continuation-first' ? 5 : order === 'parent-first' ? -5 : 0);
      const parentFirst = order !== 'tie-parent-last';
      let armed = false, targetTimer: any, parentTimer: any, attempts = 0;
      const addTimer = (kind: string, requested: number, actual: number, callback: () => void) => {
        const timer: any = realTimer(() => { timeline.push(kind); callback(); }, actual);
        owned.push(timer);
        const record = {kind, requested, start: timer._idleStart, delay: timer._idleTimeout};
        timers.push(record); handles.set(record, timer); return record;
      };
      const armParent = () => { parentTimer = addTimer('parent', parentMs, parentMs, () => parent.abort()); };
      const overdue = () => { const until = performance.now() + 55; while (performance.now() < until) {} };
      const armTarget = (kind: string, requested: number, callback: () => void) => {
        assert.ok(!armed, id); armed = true;
        if (parentFirst) armParent();
        targetTimer = addTimer(kind, requested, operationMs, callback);
        if (!parentFirst) armParent();
        overdue();
      };
      // Keep the actual TS timeout/retry arguments visible. Only wall-clock waits
      // are compressed; the replay uses the original logical timeout durations.
      globalThis.setTimeout = ((callback: any, ms: number, ...args: any[]) => {
        if (retryPhase && ms === 500 && !armed) {
          armTarget('retry', ms, () => callback(...args));
          return handles.get(targetTimer);
        }
        if (path === 'web_fetch' && event === 'idle' && !bodyPhase && ms === 60000 && !armed && (phase !== 'redirect-hop' || requests.length > 0)) {
          // Arm before the first (outer hop) timeout, not after fetch starts.
          armed = true;
          if (parentFirst) armParent();
          targetTimer = addTimer('watchdog', ms, 40, () => callback(...args));
          if (!parentFirst) armParent();
          return handles.get(targetTimer);
        }
        const actual = ms === 60000 ? (event === 'idle' && !bodyPhase ? 40 : 1000) : ms;
        const record = addTimer('timer', ms, actual, () => callback(...args));
        return handles.get(record);
      }) as any;
      const dispatcher = { dispatch(opts: any, handler: any) {
        assert.equal(String(opts.origin), 'https://fixture.test', 'no external traffic');
        requests.push(opts.path);
        if (opts.path === '/start') attempts++;
        let ended = false;
        const controller = {
          rawHeaders: [] as Buffer[],
          pause() {}, resume() {},
          abort(reason: any) { if (!ended) { ended = true; handler.onResponseError(controller, reason); } },
        };
        handler.onRequestStart(controller);
        const headers = (status: number, extra: any = {}) => {
          if (!ended) {
            const values = {'content-type': 'text/plain', ...extra};
            controller.rawHeaders = Object.entries(values).flatMap(([key, value]) => [Buffer.from(key), Buffer.from(String(value))]);
            handler.onResponseStart(controller, status, values, 'OK');
          }
        };
        const complete = () => {
          if (!ended) { handler.onResponseData(controller, Buffer.from('FIRST')); ended = true; handler.onResponseEnd(controller, {}); }
        };
        const reject = () => controller.abort(new TypeError('fixture transport failure'));
        queueMicrotask(() => {
          if (ended) return;
          if (retryPhase && attempts === 1) {
            if (phase === 'retry-error') reject();
            else { headers(503); complete(); }
            return;
          }
          if (retryPhase) {
            if (path === 'fetch_redirect' && opts.path === '/start') headers(302, {location: '/next'});
            else { headers(200); complete(); }
            return;
          }
          if (phase === 'redirect-hop' && opts.path === '/start') {
            headers(302, {location: '/next'});
            // The predecessor body intentionally never completes.
            return;
          }
          if (bodyPhase) headers(200);
          const settle = () => {
            if (ended) return;
            if (event === 'fulfilled') { if (!bodyPhase) headers(200); complete(); }
            else reject();
          };
          if (armed) { overdue(); return; } // web headers idle: real hop watchdog
          // Custom dispatcher bypasses Undici Client timers, including web body.
          // This watchdog is injected, not an intercepted 600000ms baseline timer.
          armTarget(event === 'idle' ? 'watchdog' : 'completion', event === 'idle' ? 600000 : 20, settle);
        });
        return true;
      }};
      setGlobalDispatcher(dispatcher);
      let result: any;
      const bounded = new Promise((_, reject) => owned.push(realTimer(() => reject(new Error(`probe deadline: ${id}`)), 3000)));
      try {
        const operation = (async () => {
          if (path === 'web_fetch') {
            const cache = {get() {return undefined;}, set() {}};
            return (await getURLMarkdownContent('https://fixture.test/start', parent.signal, {cache} as any)).content;
          }
          const response = retryPhase
            ? await networkFetch('https://fixture.test/start', {}, {signal: parent.signal, timeoutMs: 60000, retry: {maxRetries: 2, baseDelayMs: 500, maxDelayMs: 5000}})
            : await fetch('https://fixture.test/start', {signal: parent.signal});
          return phase === 'text-body' ? await response.text() : Buffer.from(await response.arrayBuffer()).toString();
        })();
        result = {name: null, message: null, delivery: 'ok', content: await Promise.race([operation, bounded])};
      } catch (error: any) {
        if (error.message.startsWith('probe deadline:')) throw error;
        result = {name: error.name, message: error.message, delivery: 'throw', content: null};
      } finally {
        globalThis.setTimeout = realTimer;
        for (const timer of owned) realClear(timer);
      }
      assert.ok(targetTimer && parentTimer, `${id}: reached target and parent; result=${JSON.stringify(result)} requests=${JSON.stringify(requests)}`);
      const targetDeadline = targetTimer.start + targetTimer.delay;
      const parentDeadline = parentTimer.start + parentTimer.delay;
      const wantedGap = order === 'continuation-first' ? 5 : order === 'parent-first' ? -5 : 0;
      if (parentDeadline - targetDeadline !== wantedGap) {
        console.error(JSON.stringify({id, attempt, discard: 'different actual Node deadlines', timers}));
        continue;
      }
      assert.equal(timers.indexOf(parentTimer) < timers.indexOf(targetTimer), parentFirst, `${id}: registration order`);
      const row = {...spec, id, timers, timeline, requests,
        observable: {...result, retries: attempts - 1, calls: requests.length},
        timing: {target: targetTimer.kind, requested: targetTimer.requested, effective: targetTimer.delay,
          parentGap: wantedGap, parentRegisteredFirst: parentFirst, source: expectedTiming(spec).source}};
      console.log(JSON.stringify(row)); captured = true; break;
    }
    assert.ok(captured, `${id}: exhausted same-deadline candidates`);
  }
} finally {
  globalThis.setTimeout = realTimer; Math.random = realRandom; setGlobalDispatcher(oldDispatcher);
}
