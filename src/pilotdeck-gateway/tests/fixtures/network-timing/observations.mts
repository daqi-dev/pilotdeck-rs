import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';

export const sources = ['src/network/fetch.ts', 'src/tool/builtin/web/urlFetcher.ts', 'src/mcp/client/McpClient.ts', 'src/extension/hooks/execution/HttpHookExecutor.ts', 'src/extension/hooks/execution/parseHookOutput.ts', 'src/agent/sub/BackgroundSubagentRuntime.ts'];
export function dependencyFingerprint(root: string) {
  const files = ['index.js', 'package.json', 'lib/web/fetch/index.js', 'lib/web/fetch/body.js', 'lib/web/fetch/request.js', 'lib/web/fetch/response.js', 'lib/dispatcher/agent.js', 'lib/dispatcher/client.js', 'lib/dispatcher/client-h1.js', '../@modelcontextprotocol/sdk/dist/esm/client/streamableHttp.js'];
  return {dependency: 'undici', version: JSON.parse(readFileSync(`${root}/node_modules/undici/package.json`, 'utf8')).version,
    sourceSha256: Object.fromEntries(files.map(path => [path, createHash('sha256').update(readFileSync(resolve(root,'node_modules/undici',path))).digest('hex')]))};
}
export function matrix() {
  const orders = ['continuation-first', 'parent-first', 'tie-parent-first', 'tie-parent-last'];
  const rows: any[] = [];
  for (const path of ['web_fetch', 'fetch_redirect', 'mcp_get', 'mcp_post', 'hooks', 'background']) {
    for (const phase of ['headers', 'body', ...(path === 'fetch_redirect' ? ['text-body'] : []), 'redirect-hop'])
      for (const event of ['fulfilled', 'rejected', 'idle']) for (const order of orders) rows.push({path, phase, event, order});
    for (const phase of (['mcp_post', 'hooks'].includes(path) ? [] : ['retry-status', 'retry-error'])) for (const order of orders) rows.push({path, phase, event: 'fulfilled', order});
  }
  for (const path of ['web_fetch','fetch_redirect','mcp_get','mcp_post','hooks','background'])
    for (const event of ['fulfilled','rejected','idle']) for (const order of orders) rows.push({path,phase:'io-headers',event,order});
  for (const event of ['pending-stop','settled-fulfilled','settled-rejected','running-uncooperative']) rows.push({path:'background',phase:'lifecycle',event,order:'ordered'});
  return rows.map(row => ({...row, id: `${row.path}/${row.phase}/${row.event}/${row.order}`}));
}

// The requested duration is validated independently of raw timers. Provenance
// distinguishes intercepted product timers from the custom dispatcher's watchdog.
export function expectedTiming(row: any) {
  const retry = row.phase.startsWith('retry-');
  const idle = row.event === 'idle';
  const realWatchdog = idle && ['web_fetch', 'mcp_get', 'mcp_post', 'background'].includes(row.path) && !['body', 'text-body'].includes(row.phase);
  return {target: retry ? 'retry' : idle ? 'watchdog' : 'completion',
    requested: retry ? 500 : realWatchdog ? (row.path === 'mcp_get' ? 10000 : 60000) : idle ? 600000 : 20,
    effective: idle ? 40 : 20,
    source: retry ? 'baseline retry timer' : realWatchdog ? (row.path === 'web_fetch' ? 'baseline hop timeout' : 'baseline request timeout')
      : idle ? 'fixture-injected watchdog' : 'fixture-injected completion'};
}

// Recheck raw timing evidence independently of the collector's acceptance test.
export function validate(observations: any[], hashes: any, dependency: any) {
  const provenance = observations.filter(row => Object.hasOwn(row, 'source'));
  assert.equal(provenance.length, sources.length, 'source coverage');
  for (const [i, row] of provenance.entries()) {
    assert.equal(row.source, sources[i], 'source order');
    assert.equal(row.sha256, hashes[sources[i]], 'source hash');
    assert.equal(row.node, 'v22.22.0', 'Node version');
  }
  const dependencies = observations.filter(row => Object.hasOwn(row, 'dependency'));
  assert.deepEqual(dependencies, [dependency], 'installed undici fingerprint');
  const rawCases = observations.filter(row => Object.hasOwn(row, 'id'));
  const expected = matrix();
  assert.equal(rawCases.length, expected.length, 'matrix size');
  assert.equal(observations.length, sources.length + 1 + rawCases.length, 'unexpected probe output');
  for (const [i, row] of rawCases.entries()) {
    for (const key of ['id', 'path', 'phase', 'event', 'order']) assert.equal(row[key], expected[i][key], `matrix ${key}`);
    if (row.phase === 'lifecycle') {
      assert.equal(row.trace[0],'initial:pending','background deferred run');
      const expected = row.event==='pending-stop' ? ['initial:pending','stop']
        : row.event==='running-uncooperative' ? ['initial:pending','run','stop','wait-pending','release']
        : ['initial:pending','run','settled','stop'];
      assert.deepEqual(row.trace,expected,'background lifecycle trace');
      assert.equal(row.observable.calls,row.trace.filter((e:string)=>e==='run').length,'lifecycle call trace');
      assert.equal(row.observable.pendingAfterStop,row.trace.includes('wait-pending'),'lifecycle pending trace');
      continue;
    }
    if (row.phase === 'io-headers') {
      assert.deepEqual(row.io,{transport:'loopback-undici',gate:row.event==='idle'?'peer-request':row.event==='fulfilled'?'onResponseStart':'onResponseError',order:row.order},'I/O provenance');
      assert.ok(row.trace.includes('peer-request'),'real peer reached');
      assert.equal(row.trace[0],'peer-request','peer before callbacks');
      for(const event of ['peer-request','gate','release','parent']) assert.equal(row.trace.filter((e:string)=>e===event).length,1,'unique I/O event');
      const observed=row.event==='idle'?'peer-request':`wire:${row.io.gate}`;
      assert.ok(row.trace.indexOf(observed)>=0 && row.trace.indexOf(observed)<row.trace.indexOf('gate'),'wire before gate');
      const release=row.trace.indexOf('release'),parent=row.trace.indexOf('parent');
      assert.ok(release>row.trace.indexOf('gate') && parent>row.trace.indexOf('gate'),'gate before dispatch');
      assert.equal(release<parent,['continuation-first','tie-parent-last'].includes(row.order),'I/O dispatch order');
      const drain=row.event==='idle'?'pending-observed':'continuations-drained';
      assert.equal(row.trace.includes(drain),row.order==='continuation-first','I/O continuation drain');
      if(row.order==='continuation-first' && row.event!=='idle') {
        const witness=row.event==='rejected' && ['web_fetch','mcp_get','background'].includes(row.path)?'retry-armed':'child-settled';
        assert.ok(row.trace.indexOf(witness)>release && row.trace.indexOf(witness)<row.trace.indexOf(drain),'actual continuation witness');
      }
      if(row.order==='continuation-first') assert.ok(release<row.trace.indexOf(drain) && row.trace.indexOf(drain)<parent,'drain before parent');
      assert.equal(row.trace.includes('parent-continuations-drained'),row.order==='parent-first','I/O parent drain');
      if(row.order==='parent-first') assert.ok(parent<row.trace.indexOf('parent-continuations-drained') && row.trace.indexOf('parent-continuations-drained')<release,'parent drained before release');
      if(row.event==='idle') assert.ok(row.trace.every((e:string,index:number)=>!e.startsWith('wire:')||index>parent),'idle cannot have a response before abort');
    } else {
    const timing = expectedTiming(row);
    assert.equal(row.timing.target, timing.target, 'expected target');
    assert.equal(row.timing.requested, timing.requested, 'expected requested');
    assert.equal(row.timing.effective, timing.effective, 'expected effective');
    assert.equal(row.timing.source, timing.source, 'expected timing source');
    const target = row.timers.filter((timer: any) => timer.kind === row.timing.target);
    const parents = row.timers.filter((timer: any) => timer.kind === 'parent');
    assert.equal(target.length, 1, 'exactly one target timer');
    assert.equal(parents.length, 1, 'exactly one parent timer');
    const t = target[0], p = parents[0];
    for (const timer of [t, p]) for (const key of ['start', 'delay', 'requested']) assert.ok(Number.isFinite(timer[key]), 'finite raw timer');
    const gap = row.order === 'continuation-first' ? 5 : row.order === 'parent-first' ? -5 : 0;
    assert.equal(p.start + p.delay - t.start - t.delay, gap, 'actual Node deadline gap');
    assert.equal(row.timers.indexOf(p) < row.timers.indexOf(t), row.order !== 'tie-parent-last', 'actual registration order');
    assert.equal(p.requested, timing.effective + gap, 'parent requested');
    assert.equal(p.delay, timing.effective + gap, 'parent effective');
    assert.deepEqual(row.timing, {target: t.kind, requested: t.requested, effective: t.delay, parentGap: gap, parentRegisteredFirst: row.order !== 'tie-parent-last', source: timing.source}, 'timing projection');
    const pi = row.timeline.indexOf('parent'), ti = row.timeline.indexOf(t.kind);
    const parentWins = row.order === 'parent-first' || row.order === 'tie-parent-first';
    assert.ok(parentWins ? pi >= 0 : ti >= 0, 'winning callback executed');
    if (pi >= 0 && ti >= 0) assert.equal(pi < ti, parentWins, 'actual callback order');
    }
    if(row.path==='hooks') {
      assert.equal(row.observable.delivery,'return','hook returns instead of throwing');
      assert.ok(row.observable.hook && typeof row.observable.hook==='object','hook result retained');
      assert.equal(row.observable.name,null,'hook has no thrown name');
      assert.equal(row.observable.content,row.observable.hook.stdout,'hook stdout projection');
      assert.equal(row.observable.message,row.observable.hook.stderr||null,'hook stderr projection');
    }
    assert.equal(row.observable.calls, row.requests.length, 'calls trace');
    assert.equal(row.observable.retries, row.requests.filter((path: string) => path === '/start').length - 1, 'retry trace');
    assert.ok(row.requests.length > 0, 'reached transport');
    if (row.phase === 'redirect-hop' || (row.path === 'fetch_redirect' && row.phase.startsWith('retry-') && row.observable.delivery === 'ok'))
      assert.ok(row.requests.includes('/next'), 'reached redirect hop');
  }
  // Only absolute timer observations/timeline are left in raw.ndjson; every
  // semantic observation, request and proven relative order stays in the golden.
  return rawCases.map(({timers, timeline, trace, ...row}) => row);
}
