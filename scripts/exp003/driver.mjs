import './preload.mjs';
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const [mode, output, baseline, project] = process.argv.slice(2);
const token = readFileSync(process.env.PILOTDECK_GATEWAY_TOKEN_PATH, 'utf8').trim();
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const deadline = async (test, label, ms = 20000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) { const result = test(); if (result) return result; await sleep(50); }
  throw new Error(`timeout: ${label}`);
};
const save = (value) => writeFileSync(output, JSON.stringify(value, null, 2) + '\n');

async function http(url, options = {}) {
  const response = await fetch(url, { ...options, signal: AbortSignal.timeout(15000) });
  const body = await response.text();
  return { url, status: response.status, headers: Object.fromEntries(response.headers), body };
}

async function uiSocket() {
  const ws = new WebSocket('ws://127.0.0.1:3301/ws');
  const frames = [];
  ws.addEventListener('message', (event) => frames.push(JSON.parse(event.data)));
  await deadline(() => ws.readyState === WebSocket.OPEN, 'UI socket open');
  return { ws, frames };
}
const command = (sessionId, text) => ({ type: 'pilotdeck-command', command: text,
  options: { projectPath: project, cwd: project, sessionId, permissionMode: 'default' } });

if (mode === 'ui') {
  const { ws, frames } = await uiSocket();
  const initial = await http('http://127.0.0.1:3301/api/projects');
  assert.equal(initial.status, 200);
  ws.send(JSON.stringify(command('web:exp003-chat', 'EXP003_TEXT')));
  await deadline(() => frames.some((frame) => frame.kind === 'complete'), 'UI complete');
  assert(frames.some((frame) => JSON.stringify(frame).includes('EXP003_MOCK_OK')), 'visible assistant text');
  assert(!frames.some((frame) => frame.kind === 'error'), 'no UI error');
  const projects = await http('http://127.0.0.1:3301/api/projects');
  const entries = JSON.parse(projects.body);
  const entry = entries.find((item) => JSON.stringify(item).includes(project));
  assert(entry, 'nonempty project list includes experiment project');
  const sessions = await http(`http://127.0.0.1:3301/api/projects/${encodeURIComponent(entry.name)}/sessions`);
  assert.equal(sessions.status, 200);
  assert(sessions.body.includes('exp003-chat'), 'session visible');
  const history = await http(`http://127.0.0.1:3301/api/sessions/${encodeURIComponent('web:exp003-chat')}/messages?projectPath=${encodeURIComponent(project)}`);
  assert.equal(history.status, 200);
  assert(history.body.includes('EXP003_MOCK_OK'), 'history contains persisted reply');
  save({ initial, projects, sessions, history, frames });
  ws.close();
} else if (mode === 'reconnect') {
  const { ws, frames } = await uiSocket();
  ws.send(JSON.stringify(command('web:exp003-reconnect', 'EXP003_RECONNECT')));
  const question = await deadline(() => frames.find((frame) => JSON.stringify(frame).includes('Continue exp003?') && frame.requestId), 'pending question');
  // The orchestrator observes this file, kills ONLY its own proxy, then restarts.
  writeFileSync(process.env.EXP003_RECONNECT_READY, JSON.stringify(question));
  try {
    await deadline(() => frames.find((frame) => String(frame.id).startsWith('interaction_replay:')), 'interaction replay', 10000);
  } catch (error) {
    save({ status: 'FAIL', error: error.message, question, frames });
    ws.close();
    throw error;
  }
  ws.send(JSON.stringify({ type: 'elicitation-response', sessionId: 'web:exp003-reconnect',
    requestId: question.requestId, answer: { type: 'answered', answers: { 'Continue exp003?': 'yes' } } }));
  await deadline(() => frames.some((frame) => frame.kind === 'complete'), 'replayed turn complete', 30000);
  assert(frames.some((frame) => JSON.stringify(frame).includes('EXP003_MOCK_OK')));
  save({ question, frames });
  ws.close();
} else if (mode === 'http') {
  const headers = { authorization: `Bearer ${token}` };
  const principal = { pilotDeckUserId: 'exp003', tenantId: 'exp003', actorUserId: 'exp003', agentId: 'exp003' };
  const records = [];
  for (const path of ['/health', '/auth/local-token', '/api/web/projects', '/api/module-host/describe',
    `/api/web/projects/${encodeURIComponent(project)}/files/read?path=note.txt`]) {
    const request = path === '/api/module-host/describe' ? { method: 'POST',
      headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ principal }) } : { headers };
    const direct = await http(`http://127.0.0.1:28790${path}`, request);
    const proxy = await http(`http://127.0.0.1:28789${path}`, request);
    assert.equal(proxy.status, direct.status);
    assert.equal(proxy.body, direct.body);
    assert.equal(proxy.headers['content-type'], direct.headers['content-type']);
    assert.equal(proxy.status, 200, path);
    records.push({ path, direct, proxy });
  }
  const path = `/api/web/projects/${encodeURIComponent(project)}/files/write`;
  const post = await http(`http://127.0.0.1:28789${path}`, { method: 'POST', headers: { ...headers, 'content-type': 'application/json' },
    body: JSON.stringify({ path: 'written.txt', content: 'full HTTP request body\n' }) });
  assert.equal(post.status, 200);
  assert.equal(readFileSync(`${project}/written.txt`, 'utf8'), 'full HTTP request body\n');
  const unauthorized = await http('http://127.0.0.1:28789/api/module-host/describe', { method: 'POST',
    headers: { 'content-type': 'application/json' }, body: JSON.stringify({ principal }) });
  assert.equal(unauthorized.status, 401);
  const streaming = [];
  for (const format of ['sse', 'ndjson']) {
    const sides = [];
    for (const port of [28790, 28789]) {
      const start = performance.now();
      const response = await fetch(`http://127.0.0.1:${port}/exp003/stream/${format}`);
      const chunks = [];
      for await (const chunk of response.body) chunks.push({ elapsedMs: performance.now() - start, text: Buffer.from(chunk).toString() });
      assert.equal(response.status, 200);
      assert(chunks.length >= 2, 'stream must arrive incrementally');
      assert(chunks.at(-1).elapsedMs - chunks[0].elapsedMs >= 350, 'first data must precede delayed terminal chunk');
      sides.push({ port, status: response.status, headers: Object.fromEntries(response.headers), chunks });
    }
    assert.equal(sides[0].chunks.map((chunk) => chunk.text).join(''), sides[1].chunks.map((chunk) => chunk.text).join(''));
    streaming.push({ format, sides });
  }
  save({ records, post, unauthorized, streaming });
} else if (mode === 'reconnect-protocol') {
  const { GatewayWsClient } = await import(pathToFileURL(`${baseline}/dist/src/gateway/client/GatewayWsClient.js`));
  const url = process.env.EXP003_GATEWAY_URL;
  const sessionKey = url.includes(':28789/') ? 'web:exp003-protocol-proxy' : 'web:exp003-protocol-direct';
  const first = new GatewayWsClient({ url, token, clientName: 'web' });
  await first.connect();
  const previousBinding = first.interactionBinding;
  const before = [];
  const stream = (async () => {
    try {
      for await (const event of first.stream('submit_turn', { sessionKey, channelKey: 'web', projectKey: project,
        message: 'EXP003_RECONNECT', runId: `run-${sessionKey}` })) before.push(event);
    } catch (error) { before.push({ driverDisconnect: error.message }); }
  })();
  const question = await deadline(() => before.find((event) => event.type === 'elicitation_request'), 'protocol pending question');
  first.close();
  await stream;
  await sleep(200);
  const second = new GatewayWsClient({ url, token, clientName: 'web' });
  await second.connect();
  const replay = await second.request('reconnect_interaction', { sessionKey, previousBinding });
  assert(replay.requests.some((request) => request.requestId === question.requestId), 'pending request survives 1:1 disconnect');
  const active = await second.request('active_turn_snapshot', { sessionKey, includeEvents: true });
  assert(active.active, 'pending turn remains active');
  const answer = await second.request('elicitation_respond', { requestId: question.requestId,
    answer: { type: 'answered', answers: { 'Continue exp003?': 'yes' } } });
  // Replay is this supplementary probe's assertion. Keep a failed post-replay
  // answer visible; never relabel the actual UI automatic replay as successful.
  if (answer.delivered === false) {
    save({ status: 'PASS-with-note', previousBinding, nextBinding: second.interactionBinding, before, replay, active, answer,
      limitation: 'baseline rejects post-reconnect elicitation answer on both direct and proxy connections' });
    second.close();
    process.exit(0);
  }
  let terminal;
  const end = Date.now() + 15000;
  while (Date.now() < end) {
    terminal = await second.request('active_turn_snapshot', { sessionKey, includeEvents: true });
    if (terminal.terminal) break;
    await sleep(100);
  }
  assert(terminal.terminal, 'replayed turn reaches terminal');
  assert(JSON.stringify(terminal).includes('EXP003_MOCK_OK'));
  save({ previousBinding, nextBinding: second.interactionBinding, before, replay, active, terminal });
  second.close();
} else if (mode === 'equivalence') {
  // Exactly the same official client and scenario in both rounds.
  const { GatewayWsClient } = await import(pathToFileURL(`${baseline}/dist/src/gateway/client/GatewayWsClient.js`));
  const client = new GatewayWsClient({ url: process.env.EXP003_GATEWAY_URL, token, clientName: 'test' });
  await client.connect();
  const results = [];
  for (const [method, params] of [['supported_agents', {}], ['output_styles_list', {}],
    ['project_files_list', { projectKey: project }]]) results.push({ method, result: await client.request(method, params) });
  const events = [];
  for await (const event of client.stream('submit_turn', { sessionKey: 'web:exp003-equivalence',
    channelKey: 'web', projectKey: project, message: 'EXP003_TEXT', runId: 'run-exp003-equivalence' })) events.push(event);
  assert(events.some((event) => event.type === 'turn_completed'), 'final turn completed');
  assert(events.some((event) => JSON.stringify(event).includes('EXP003_MOCK_OK')));
  save({ results, events });
  client.close();
} else if (mode === 'close') {
  const cases = [
    { expected: 4001, frame: { type: 'request', id: 'no-hello', method: 'describe_server', params: {} } },
    { expected: 4001, frame: { type: 'hello', protocolVersion: 'wrong', clientName: 'test', clientVersion: '0.1.0', token } },
    { expected: 4003, frame: { type: 'hello', protocolVersion: '1.2', clientName: 'test', clientVersion: '0.1.0', token: 'wrong' } },
    { expected: 4002, raw: '{invalid json' },
  ];
  const result = [];
  for (const test of cases) {
    const ws = new WebSocket('ws://127.0.0.1:28789/ws');
    const closed = new Promise((resolve) => ws.addEventListener('close', (event) => resolve({ code: event.code, reason: event.reason })));
    await deadline(() => ws.readyState === WebSocket.OPEN, 'close test open');
    ws.send(test.raw ?? JSON.stringify(test.frame));
    const close = await Promise.race([closed, sleep(10000).then(() => { throw new Error('close timeout'); })]);
    assert.equal(close.code, test.expected);
    result.push({ ...test, close });
  }
  for (const code of [undefined, 4444]) {
    const ws = new WebSocket('ws://127.0.0.1:28789/ws');
    const closed = new Promise((resolve) => ws.addEventListener('close', (event) => resolve({ code: event.code, reason: event.reason })));
    await deadline(() => ws.readyState === WebSocket.OPEN, 'client close open');
    ws.close(code, code ? 'client_reason' : undefined);
    const close = await closed;
    assert.equal(close.code, 1000, 'return actual TS reply, including empty and custom client close');
    assert.equal(close.reason, '');
    result.push({ clientClose: code ?? 'empty', close });
  }
  save(result);
} else throw new Error(`unknown mode: ${mode}`);
