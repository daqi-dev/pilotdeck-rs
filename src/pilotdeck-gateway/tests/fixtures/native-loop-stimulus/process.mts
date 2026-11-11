// #1371 bounded executable probe for the controlled AgentLoop stimulus. Real
// gateway binary, real sidecar binary, real WS client, loopback provider;
// every scenario owns an isolated home/project and OS-assigned ports. The
// observer is also the collector: it arms `loop.json`, watches the product's
// witness receipts and writes `release.json` (atomically, tmp + rename) at the
// exact boundary each completion-ordering cell names.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir, readFile, writeFile, readdir, rename, stat} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {once} from 'node:events';
const require = createRequire(join(process.env.PDRS_BASELINE!, 'package.json'));
const WebSocket = require('ws');
assert.equal(process.version, 'v22.22.0', '#1371 pinned Node');
const [binary, sidecar, out] = process.argv.slice(2).map(p => resolve(p));
await mkdir(out, {recursive: true});
let assertionFailure: string | null = null;
const rows: any[] = [];
const children: any[] = [];
const sockets = new Set<any>();
const clients: any[] = [];
const overallDeadline = Date.now() + 180000;
const sha = async (path: string) => createHash('sha256').update(await readFile(path)).digest('hex');
const identities = {binary: {path: binary, sha256: await sha(binary)}, sidecar: {path: sidecar, sha256: await sha(sidecar)}};
const bounded = async <T>(promise: Promise<T>, label: string, ms = 20000, cleanup = false): Promise<T> => {
  if (!cleanup) { ms = Math.min(ms, overallDeadline - Date.now()); if (ms <= 0) throw Error('#1371 overall process deadline'); }
  let timer: any;
  try { return await Promise.race([promise, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Error('#1371 ' + label)), ms); })]); }
  finally { clearTimeout(timer); }
};
const sleep = (ms: number) => new Promise<void>(r => setTimeout(r, ms));
// Non-stream provider calls are session-title requests. An armed loop entry
// must suppress them: the controlled loop emits no assistant text, so a title
// request would be a non-comparable extra stimulus (RESULT.md TEXT row).
let titleRequests = 0;
// The original scenarios keep their existing title policy. The explicit
// provider-armed scene also witnesses the real model POST before its release.
const modelRequests: {path: string | undefined; raw: string}[] = [];
const server = createServer(async (req, res) => {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  const raw = Buffer.concat(chunks).toString();
  const body = JSON.parse(raw);
  if (!body.stream) {
    titleRequests += 1;
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({id: 'native-title', choices: [{message: {role: 'assistant', content: '{"title":"Native loop stimulus probe"}'}, finish_reason: 'stop'}]}));
    return;
  }
  modelRequests.push({path: req.url, raw});
  res.setHeader('content-type', 'text/event-stream');
  const emit = (delta: any, finish_reason: any = null) =>
    res.write('data: ' + JSON.stringify({id: 'native-probe', model: 'mock', choices: [{index: 0, delta, finish_reason}]}) + '\n\n');
  emit({role: 'assistant', content: 'native loop stimulus text'});
  emit({}, 'stop');
  res.end('data: [DONE]\n\n');
});
server.on('connection', s => { sockets.add(s); s.on('error', () => {}); s.on('close', () => sockets.delete(s)); });
server.on('clientError', (_error, socket) => { socket.destroy?.(); });
await new Promise<void>(r => server.listen(0, '127.0.0.1', r));
const port = (server.address() as any).port;

async function launch(name: string, control?: string) {
  const home = join(out, name, 'home'), project = join(out, name, 'project');
  await mkdir(home, {recursive: true}); await mkdir(project, {recursive: true});
  await writeFile(join(home, 'pilotdeck.yaml'), `schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:${port}/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n`, {flag: 'wx'});
  const token = join(home, 'token');
  await writeFile(token, 'native-loop-stimulus-fixture\n', {flag: 'wx'});
  const args = ['--native', '--home', home, '--project', project, '--listen', '127.0.0.1:0', '--token-file', token, '--sidecar', sidecar, '--drain-ms', '1500'];
  const env: any = {PATH: process.env.PATH, HOME: home, PILOT_HOME: home, LANG: 'en_US.UTF-8'};
  if (control !== undefined) env.PILOTDECK_NATIVE_LOOP_STIMULUS = control;
  const child = spawn(binary, args, {cwd: project, env, stdio: ['ignore', 'pipe', 'pipe']});
  children.push(child);
  let stderr = '';
  const exit = once(child, 'exit');
  child.stderr.on('data', (b: any) => stderr += b.toString());
  const ready = bounded(new Promise<string>((resolve, reject) => {
    child.stderr.on('data', () => { const match = /native listening (\S+:\d+)[^\n]*\n/.exec(stderr); if (match) resolve(match[1]); });
    child.once('exit', (code: any) => reject(Error(`#1371 early exit ${code}: ${stderr}`)));
    child.once('error', reject);
  }), 'readiness');
  const address = await ready;
  rows.push({name, pid: child.pid, argv: [binary, ...args], address});
  return {child, exit, address, project, home, stderr: () => stderr};
}

async function client(address: string) {
  const ws = new WebSocket(`ws://${address}/ws`);
  clients.push(ws);
  let failure: Error | undefined;
  const queue: any[] = [];
  let wake: (() => void) | undefined;
  ws.on('error', (e: Error) => { failure = e; wake?.(); });
  ws.on('close', () => { failure ??= Error('#1371 WS closed'); wake?.(); });
  ws.on('message', (b: any) => { queue.push(JSON.parse(b)); wake?.(); wake = undefined; });
  await bounded(new Promise<void>((resolve, reject) => { ws.once('open', () => resolve()); ws.once('error', reject); }), 'WS open');
  const next = () => bounded((async () => {
    while (!queue.length) { if (failure) throw failure; await new Promise<void>(r => wake = r); }
    const value = queue.shift();
    rows.push({frame: value});
    return value;
  })(), 'WS response');
  ws.send(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'ui', clientVersion: 'native-loop-stimulus', token: 'native-loop-stimulus-fixture'}));
  assert.equal((await next()).type, 'hello_ok', '#1371 real authentication');
  return {ws, next};
}

// The turn timeout every cell arms. A cell that claims "the release decided
// the turn, not the timer" has to observe its terminal inside this window:
// a timer-first turn lands past it (the U-T cell records ~432 ms), so the
// bound separates the two outcomes instead of accepting both.
const TURN_TIMEOUT_MS = 400;
// Collector-side control protocol: atomic loop.json / release.json, and a
// bounded poll over the product's witness directory.
const writeAtomic = async (path: string, text: string) => { const tmp = path + '.tmp'; await writeFile(tmp, text); await rename(tmp, path); };
const arm = async (control: string, plan: any) => writeAtomic(join(control, 'loop.json'), JSON.stringify(plan));
const release = async (control: string, plan: any, stage: string, outcome?: string) =>
  writeAtomic(join(control, 'release.json'), JSON.stringify({id: plan.id, stage, ...(outcome ? {outcome} : {})}));
const finished = (files: string[]) => files.filter(name => name.endsWith('.json'));
// The file itself is needed for cells that order two receipts across
// processes: their `seq` counters are independent, so only the write time is
// comparable.
const witnessPath = async (control: string, site: string, phase: string, key: string, ms = 15000) => {
  const dir = join(control, 'witness');
  const deadline = Math.min(Date.now() + ms, overallDeadline);
  for (;;) {
    const files = finished((await readdir(dir).catch(() => [] as string[])).sort());
    for (const name of files) {
      const path = join(dir, name);
      const receipt = JSON.parse(await readFile(path, 'utf8'));
      if (receipt.site === site && receipt.phase === phase && receipt.key === key) return path;
    }
    if (Date.now() > deadline) assert.ok(false, `#1371 no ${site} ${phase} receipt for key ${key}`);
    await sleep(10);
  }
};
const witness = async (control: string, site: string, phase: string, key: string, ms = 15000) =>
  JSON.parse(await readFile(await witnessPath(control, site, phase, key, ms), 'utf8'));
const hasWitness = (control: string, site: string, phase: string, key: string) =>
  witness(control, site, phase, key, 200).then(() => true, () => false);
const exists = async (path: string) => readdir(path).then(() => true, () => false);
const submitParams = (process: any, name: string, extra: any = {}) => ({
  sessionKey: `${name}-session`, runId: `${name}-run`, projectKey: process.project,
  channelKey: 'probe', message: `${name} native loop stimulus turn`, allowedTools: [], ...extra,
});
// Drain frames for one submit until its final frame. Returns the collected
// event frames (excluding the final repeat).
const submitUntilFinal = async (ws: any, next: () => Promise<any>, name: string, process: any, extra: any = {}) => {
  const started = Date.now();
  ws.send(JSON.stringify({type: 'request', id: 'submit', method: 'submit_turn', params: submitParams(process, name, extra)}));
  const events: any[] = [];
  const at: number[] = [];
  for (;;) {
    const frame = await next();
    assert.equal(frame.type, 'event', `#1371 ${name} submit event envelope`);
    assert.equal(frame.id, 'submit', `#1371 ${name} submit identity`);
    assert.equal(frame.seq, events.length, `#1371 ${name} submit sequence`);
    at.push(Date.now() - started);
    if (frame.final) return {events, at, terminal: frame.event};
    events.push(frame.event);
  }
};
const stop = async (process: any, label: string) => {
  process.child.kill('SIGTERM');
  const status = await bounded(process.exit, `${label} signal drain`);
  assert.deepEqual(status, [0, null], `#1371 ${label} clean signal exit`);
};

try {
  // Unset: the production default. No receipt, no control reads, the real loop.
  {
    const titlesBefore = titleRequests;
    const process = await launch('loop-unset');
    assert.ok(!process.stderr().includes('native loop stimulus on'), '#1371 unset keeps no stimulus receipt');
    const {ws, next} = await client(process.address);
    const {events, terminal} = await submitUntilFinal(ws, next, 'loop-unset', process);
    assert.equal(terminal.type, 'turn_completed', '#1371 unset turn completes');
    // The label claims model *text*, so the turn_completed event (which any
    // completed turn emits, text or not) cannot satisfy it: the provider's
    // own string must arrive as an event text payload.
    const texts = events.filter((e: any) => typeof e.text === 'string' && e.text.length > 0);
    assert.ok(texts.some((e: any) => e.text.includes('native loop stimulus text')), '#1371 unset real model text');
    // The title request is asynchronous and `titleRequests` is one run-wide
    // counter: settle this scenario's own request before the next scenario
    // takes its snapshot, or a late arrival would be credited to that
    // scenario and could mask a suppressed title there.
    const unsetTitleDeadline = Date.now() + 10000;
    while (titleRequests === titlesBefore && Date.now() < unsetTitleDeadline) await sleep(25);
    assert.ok(titleRequests > titlesBefore, '#1371 unset keeps the title request');
    rows.push({scenario: 'unset', eventTypes: [...new Set(events.map((e: any) => e.type))], titles: titleRequests - titlesBefore});
    ws.terminate(); await stop(process, 'unset');
  }
  // Armed with an existing control directory that carries no plan: the entry
  // is inert, so the real loop and its text still run.
  {
    const control = join(out, 'loop-armed-empty', 'control');
    await mkdir(control, {recursive: true});
    const titlesBefore = titleRequests;
    const process = await launch('loop-armed-empty', control);
    assert.ok(process.stderr().includes(`native loop stimulus on ${control}`), '#1371 stimulus receipt on stderr');
    const {ws, next} = await client(process.address);
    const {events, terminal} = await submitUntilFinal(ws, next, 'loop-armed-empty', process);
    assert.equal(terminal.type, 'turn_completed', '#1371 armed inert turn completes');
    assert.equal(await exists(join(control, 'witness')), false, '#1371 armed inert publishes no receipts');
    const inertTexts = events.filter((e: any) => typeof e.text === 'string' && e.text.length > 0);
    assert.ok(inertTexts.some((e: any) => e.text.includes('native loop stimulus text')), '#1371 inert turn real model text');
    // Inert keeps the production behaviour byte for byte, the automatic title
    // request included; suppressing it for an entry that never drives the
    // loop would still be a behaviour difference. The window matches the unset
    // scenario's: the request is asynchronous and a short window would turn a
    // slow but correct production path into a red cell.
    const titleDeadline = Date.now() + 10000;
    while (titleRequests === titlesBefore && Date.now() < titleDeadline) await sleep(25);
    assert.ok(titleRequests > titlesBefore, '#1371 inert turn keeps the title request');
    rows.push({scenario: 'armed-empty', eventTypes: [...new Set(events.map((e: any) => e.type))]});
    ws.terminate(); await stop(process, 'armed-empty');
  }
  // Invalid: a value that does not name an existing directory fails closed at
  // startup, before any listening advertisement.
  {
    const home = join(out, 'loop-invalid', 'home'), project = join(out, 'loop-invalid', 'project');
    await mkdir(home, {recursive: true}); await mkdir(project, {recursive: true});
    await writeFile(join(home, 'pilotdeck.yaml'), `schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture: {protocol: openai, url: "http://127.0.0.1:${port}/v1", apiKey: mock-only, models: {mock: {}}}\n`, {flag: 'wx'});
    const token = join(home, 'token');
    await writeFile(token, 'native-loop-stimulus-fixture\n', {flag: 'wx'});
    const child = spawn(binary, ['--native', '--home', home, '--project', project, '--listen', '127.0.0.1:0', '--token-file', token, '--sidecar', sidecar, '--drain-ms', '1500'], {
      cwd: project,
      env: {PATH: process.env.PATH, HOME: home, PILOT_HOME: home, LANG: 'en_US.UTF-8', PILOTDECK_NATIVE_LOOP_STIMULUS: join(out, 'loop-invalid', 'missing-control')},
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    children.push(child);
    let stderr = '';
    child.stderr.on('data', b => stderr += b);
    const status = await bounded(once(child, 'exit'), 'invalid startup').catch(() => null);
    assert.ok(status !== null && status[0] !== 0, '#1371 invalid stimulus dir rejected');
    assert.ok(stderr.includes('existing directory'), '#1371 invalid stimulus dir reports the gate');
    assert.ok(!stderr.includes('native listening'), '#1371 invalid stimulus dir never advertises ready');
    rows.push({scenario: 'invalid', status, stderr});
  }
  // TOOL row: the tool phase prefix. `unknown-first` releases the outcome
  // immediately, so the turn ends before the 400 ms timer and the recorded
  // prefix is exactly model_request_started + controlled_wait.
  {
    const control = join(out, 'loop-tool', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'tool-1', phase: 'tool', order: 'unknown-first'};
    await arm(control, plan);
    // The counter is run-wide: the two earlier real turns each generate a
    // legitimate title. Compare against a snapshot so this asserts the armed
    // controlled turn adds none of its own.
    const titlesBefore = titleRequests;
    const process = await launch('loop-tool', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-tool', process, {timeoutMs: 400});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    await witness(control, 'loop', 'wait', plan.id);
    await release(control, plan, 'outcome', 'unknown');
    const {events, terminal} = await pump;
    assert.ok(events.some(e => e.type === 'model_request_started'), '#1371 tool phase model prefix');
    assert.ok(events.some(e => e.type === 'tool_call_started' && e.name === 'controlled_wait'), '#1371 tool phase controlled_wait prefix');
    assert.equal(terminal.type, 'error', '#1371 tool phase unknown terminal');
    assert.ok(!events.some(e => e.type === 'error' && e.code === 'turn_timeout'), '#1371 tool phase no timeout');
    await witness(control, 'loop', 'final', 'result_unknown');
    ws.terminate(); await stop(process, 'tool-phase');
    assert.equal(titleRequests - titlesBefore, 0, '#1371 armed loop suppresses the automatic title request');
    rows.push({scenario: 'tool-phase', seq: events.map(e => e.type)});
  }
  // Cell 1 (U-T): loop waiting, timeout first, outer fence still current.
  {
    const control = join(out, 'loop-timeout-first', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'ut-1', phase: 'model', order: 'timeout-first'};
    await arm(control, plan);
    const process = await launch('loop-timeout-first', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-timeout-first', process, {timeoutMs: 400});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    await witness(control, 'loop', 'wait', plan.id);
    const {events, at, terminal} = await pump;
    rows.push({scenario: 'timeout-first', seq: events.map((e: any) => e.type), at, terminal, stderr: process.stderr()});
    // The timeout settlement itself is A19-HOST-001 (`specs/gateway-parity/
    // diffs/A19-HOST-001.md`): under a registry the enqueued timeout frames
    // are dropped and the Rust host has not implemented D4-B yet, so the
    // client sees the bounded `result_unknown` fallback. This cell measures
    // the loop-side ordering, not that settlement: the turn must end red and
    // bounded, never hang.
    assert.equal(terminal.type, 'error', '#1371 timeout-first ends red');
    assert.ok(at[at.length - 1] < 5000, '#1371 timeout-first stays bounded');
    assert.equal(await hasWitness(control, 'fence', 'current', 'true'), true, '#1371 timeout-first outer fence current');
    await release(control, plan, 'outcome', 'unknown');
    await witness(control, 'loop', 'final', 'result_unknown');
    assert.ok(!events.some((e: any) => e.type === 'turn_completed'), '#1371 timeout-first late unknown is not a success');
    ws.terminate(); await stop(process, 'timeout-first');
  }
  // Cell 2 (U-F): unknown first, before the timer.
  {
    const control = join(out, 'loop-unknown-first', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'uf-1', phase: 'model', order: 'unknown-first'};
    await arm(control, plan);
    const process = await launch('loop-unknown-first', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-unknown-first', process, {timeoutMs: TURN_TIMEOUT_MS});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    await witness(control, 'loop', 'wait', plan.id);
    await release(control, plan, 'outcome', 'unknown');
    const {events, at, terminal} = await pump;
    rows.push({scenario: 'unknown-first', seq: events.map((e: any) => e.type), at, terminal, events});
    // A `result_unknown` terminal must never be the reported terminal and
    // must never be classified as a timeout. The gateway still projects a
    // `turn_completed` *event* at the end of the loop; the connection layer
    // prefers the error event as the terminal, which is what is asserted.
    // The exact code is the gateway's current terminal-shape verdict
    // (`live.rs` `finish` on a payload that is not a TurnResult), recorded
    // here rather than asserted narrowly.
    assert.equal(terminal.type, 'error', '#1371 unknown-first terminal is an error');
    // The turn timeout drops its closing frames (`A19-HOST-001`), so the
    // absence of a `turn_timeout` code cannot distinguish the two races: the
    // arrival time can. Measured margins on this fixture: a release-decided
    // turn terminals around 60 ms, a timer-decided one around 430 ms (the U-T
    // cell), so this bound keeps roughly a 6x margin. A host slow enough to
    // break it also breaks the U-T cell, which is the honest reading: the
    // release did not win the race.
    assert.ok(at[at.length - 1] < TURN_TIMEOUT_MS, '#1371 unknown-first precedes the timer');
    assert.ok(!events.some((e: any) => e.type === 'error' && e.code === 'turn_timeout'), '#1371 unknown-first timer cleared');
    await witness(control, 'loop', 'final', 'result_unknown');
    ws.terminate(); await stop(process, 'unknown-first');
  }
  // Cell 3 (K-F): known final first, before the timer.
  {
    const control = join(out, 'loop-known-final-first', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'kf-1', phase: 'model', order: 'known-final-first'};
    await arm(control, plan);
    const process = await launch('loop-known-final-first', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-known-final-first', process, {timeoutMs: 400});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    await witness(control, 'loop', 'wait', plan.id);
    await release(control, plan, 'outcome', 'known');
    const {events, terminal} = await pump;
    assert.equal(terminal.type, 'turn_completed', '#1371 known-final-first completes');
    assert.ok(!events.some(e => e.type === 'error' && e.code === 'turn_timeout'), '#1371 known-final-first timer cleared');
    await witness(control, 'loop', 'final', 'completed');
    rows.push({scenario: 'known-final-first'});
    ws.terminate(); await stop(process, 'known-final-first');
  }
  // Cell 4 (A-C): already timed out, client done, fence.complete, late unknown.
  {
    const control = join(out, 'loop-after-complete', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'ac-1', phase: 'model', order: 'after-complete'};
    await arm(control, plan);
    const process = await launch('loop-after-complete', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-after-complete', process, {timeoutMs: 400});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    await witness(control, 'loop', 'wait', plan.id);
    const {events, at, terminal} = await pump;
    rows.push({scenario: 'after-complete', seq: events.map((e: any) => e.type), at, terminal});
    // As in timeout-first the timeout settlement is A19-HOST-001, so the
    // cell measures the loop/fence ordering: the turn ends red and bounded,
    // and the outer fence has already completed before the late unknown.
    assert.equal(terminal.type, 'error', '#1371 after-complete ends red');
    // The ordering is measured on the receipt files themselves, not by
    // comparing sequence numbers: the gateway and the sidecar number their
    // receipts independently. Capture the fence receipt path now, then let
    // the late unknown land, and require the fence file to be older.
    const fenceReceipt = await witnessPath(control, 'fence', 'complete', 'loop-after-complete-run');
    await release(control, plan, 'outcome', 'unknown');
    const unknownReceipt = await witnessPath(control, 'loop', 'final', 'result_unknown');
    const [fenceAt, unknownAt] = await Promise.all([stat(fenceReceipt), stat(unknownReceipt)]);
    assert.ok(fenceAt.mtimeMs < unknownAt.mtimeMs, '#1371 after-complete fence completes before the late unknown');
    ws.terminate(); await stop(process, 'after-complete');
  }
  // Cell 5 (K-P): the loop yielded completed but its return is still held, and
  // the timeout happens inside that window. Nothing may reach the client as
  // completed before the return releases.
  {
    const control = join(out, 'loop-return-pending', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'kp-1', phase: 'model', order: 'known-final-before-timeout-pending'};
    await arm(control, plan);
    const process = await launch('loop-return-pending', control);
    const {ws, next} = await client(process.address);
    const frames: any[] = [];
    const drain = (async () => { for (;;) frames.push(await next()); })();
    drain.catch(() => {});
    ws.send(JSON.stringify({type: 'request', id: 'submit', method: 'submit_turn', params: submitParams(process, 'loop-return-pending', {timeoutMs: 400})}));
    await witness(control, 'loop', 'wait', plan.id);
    await release(control, plan, 'outcome', 'known');
    await witness(control, 'loop', 'return', 'block');
    assert.equal(await hasWitness(control, 'loop', 'final', 'completed'), false, '#1371 return-pending completed is not published before the release');
    assert.ok(!frames.some(f => f.id === 'submit' && f.event?.type === 'turn_completed'), '#1371 return-pending completed stays buffered');
    // The timeout terminal is A19-HOST-001 bounded red, as in the other cells.
    await bounded((async () => { while (!frames.some(f => f.id === 'submit' && f.final)) await sleep(10); })(), 'return-pending final');
    const terminal = frames.find(f => f.id === 'submit' && f.final);
    assert.equal(terminal.event.type, 'error', '#1371 return-pending ends red');
    await release(control, plan, 'return');
    await witness(control, 'loop', 'return', 'release');
    assert.equal(await hasWitness(control, 'loop', 'bound', 'return'), false, '#1371 return-pending released inside the bound');
    // The buffered completed must not surface as a live success after the
    // timeout terminal: it is recorded, not re-delivered. The transport still
    // accepts the emission here (a deadline is not a retirement), so this cell
    // says nothing about the suppressed branch; the next cell measures it.
    // The buffered completed must not surface as a live success after the
    // timeout terminal: it is recorded, not re-delivered.
    await sleep(300);
    assert.ok(!frames.some(f => f.id === 'submit' && f.event?.type === 'turn_completed'), '#1371 return-pending late release is not a live success');
    rows.push({scenario: 'return-pending', seq: frames.filter(f => f.id === 'submit' && f.type === 'event').map(f => f.seq), terminal: terminal.event});
    ws.terminate(); await stop(process, 'return-pending');
  }
  // Bounded: without a release the loop must expire at its bound and fail the
  // command, never hang the gateway. The loop outlives the client timeout:
  // its bound expires 500 ms after the timeout terminal.
  {
    const control = join(out, 'loop-bound', 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'bd-1', phase: 'model', order: 'timeout-first', boundMs: 900};
    await arm(control, plan);
    const process = await launch('loop-bound', control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, 'loop-bound', process, {timeoutMs: 400});
    // A pump that is not awaited yet must not become an unhandled rejection:
    // that would kill the process before the record is written and turn a
    // named failure into an INVALID with no evidence.
    pump.catch(() => {});
    const wait = await witness(control, 'loop', 'wait', plan.id);
    const {terminal} = await pump;
    assert.equal(terminal.type, 'error', '#1371 bound timeout still ends red');
    const bound = await witness(control, 'loop', 'bound', 'outcome');
    // Both receipts come from the sidecar's single counter, so the expiry is
    // ordered after the wait it replaced.
    assert.ok(bound.seq > wait.seq, '#1371 bound expires instead of hanging');
    assert.equal(await hasWitness(control, 'loop', 'final', 'completed'), false, '#1371 bound never fabricates a completion');
    rows.push({scenario: 'bound', boundSeq: bound.seq, terminal});
    ws.terminate(); await stop(process, 'bound');
  }
  // #1314: the typed opt-in must reach the real host model port before the
  // existing known outcome can be released. A model-free loop can still reach
  // loop.wait, so the count and consumer receipt are separate requirements.
  {
    const name = 'loop-provider-armed';
    const control = join(out, name, 'control');
    await mkdir(control, {recursive: true});
    const plan = {id: 'provider-1', phase: 'model', order: 'known-final-first', requireProvider: true};
    await arm(control, plan);
    const before = modelRequests.length;
    const process = await launch(name, control);
    const {ws, next} = await client(process.address);
    const pump = submitUntilFinal(ws, next, name, process);
    pump.catch(() => {});
    const waiting = await witness(control, 'loop', 'wait', plan.id);
    const requests = modelRequests.slice(before);
    const consumed = await witness(control, 'model', 'completed', plan.id, 200).catch(() => null);
    await release(control, plan, 'outcome', 'known');
    const {terminal} = await pump;
    ws.terminate(); await stop(process, 'provider-armed');
    assert.ok(requests.length === 1 && requests[0].path === '/v1/chat/completions' &&
      requests[0].raw.includes(name + ' native loop stimulus turn'),
    '#1371 provider-armed loop reaches real model');
    assert.ok(consumed?.pid === waiting.pid && consumed.seq < waiting.seq,
      '#1371 provider completion precedes the wait');
    assert.equal(terminal.type, 'turn_completed', '#1371 provider-armed known outcome completes');
    rows.push({scenario: 'provider-armed', modelRequests: requests.length, completed: consumed, waiting, terminal});
  }
} catch (error: any) {
  // Only a named assertion can become a target receipt. A network error,
  // process failure or deadline stays an unclassified INVALID.
  if (error?.code === 'ERR_ASSERTION') {
    const first = String(error.message).split('\n')[0];
    if ([
      '#1371 unset keeps no stimulus receipt',
      '#1371 unset turn completes',
      '#1371 unset real model text',
      '#1371 unset keeps the title request',
      '#1371 stimulus receipt on stderr',
      '#1371 armed inert turn completes',
      '#1371 inert turn real model text',
      '#1371 inert turn keeps the title request',
      '#1371 armed inert publishes no receipts',
      '#1371 invalid stimulus dir rejected',
      '#1371 invalid stimulus dir reports the gate',
      '#1371 invalid stimulus dir never advertises ready',
      '#1371 tool phase model prefix',
      '#1371 tool phase controlled_wait prefix',
      '#1371 tool phase unknown terminal',
      '#1371 tool phase no timeout',
      '#1371 armed loop suppresses the automatic title request',
      '#1371 timeout-first ends red',
      '#1371 timeout-first stays bounded',
      '#1371 timeout-first outer fence current',
      '#1371 timeout-first late unknown is not a success',
      '#1371 unknown-first terminal is an error',
      '#1371 unknown-first precedes the timer',
      '#1371 unknown-first timer cleared',
      '#1371 known-final-first completes',
      '#1371 known-final-first timer cleared',
      '#1371 after-complete ends red',
      '#1371 after-complete fence completes before the late unknown',
      '#1371 no fence complete receipt for key loop-after-complete-run',
      '#1371 return-pending completed stays buffered',
      '#1371 return-pending completed is not published before the release',
      '#1371 return-pending ends red',
      '#1371 return-pending released inside the bound',
      '#1371 return-pending late release is not a live success',
      '#1371 bound timeout still ends red',
      '#1371 bound expires instead of hanging',
      '#1371 bound never fabricates a completion',
      '#1371 provider-armed loop reaches real model',
    ].includes(first)) assertionFailure = first;
  }
  throw error;
} finally {
  for (const ws of clients) ws.terminate();
  for (const child of children) if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await bounded(once(child, 'exit'), 'forced cleanup', 5000, true).catch(() => {});
  }
  for (const socket of sockets) socket.destroy();
  await new Promise<void>(r => server.close(() => r()));
  const after = {binary: await sha(binary), sidecar: await sha(sidecar)};
  assert.equal(after.binary, identities.binary.sha256, '#1371 binary unchanged');
  assert.equal(after.sidecar, identities.sidecar.sha256, '#1371 sidecar unchanged');
  await writeFile(join(out, 'process-record.json'), JSON.stringify({node: process.version, ...identities, after, rows, assertionFailure}, null, 2) + '\n', {flag: 'wx'});
}
console.log('#1371 native loop stimulus unset, inert, invalid and ordering cells passed');
