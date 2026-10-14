// S2 six scenarios. Raw evidence is saved before parsing or asserting.
import assert from 'node:assert/strict';
import http from 'node:http';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import crypto from 'node:crypto';
const [output, baseline, project, side, entryPort, directPort, home] = process.argv.slice(2);
const require = createRequire(join(baseline, 'package.json'));
const WS = require('ws');
const entry = Number(entryPort), direct = Number(directPort);
assert(entry !== direct, 'out-of-band observer must bypass recorder and DUT');
const definitions = JSON.parse(readFileSync(new URL('./scenarios.json', import.meta.url))).http_cases;
const started = process.hrtime.bigint();
let seq = 0, token = '', connection = 0, userId, currentScenario = 'setup';
const sockets = new Set();
function emit(file, value) {
  appendFileSync(join(output, file), JSON.stringify({ ...value, side, seq: ++seq,
    arrival_ns: String(process.hrtime.bigint() - started) }) + '\n');
}
const wait = (ms) => new Promise(resolve => setTimeout(resolve, ms));
async function until(test, label, ms = 45000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (test()) return; await wait(25); }
  throw new Error(`timeout: ${label}`);
}
function request(caseId, path, { method = 'GET', body, headers = {}, expected, onChunk } = {}) {
  const definition = definitions.find(c => c.id === caseId);
  assert(definition, `case ${caseId} must be registered`);
  expected ??= definition.expected_statuses;
  assert.deepEqual(expected, definition.expected_statuses, `case ${caseId} expected status must match manifest`);
  if (body && !Buffer.isBuffer(body)) { body = Buffer.from(JSON.stringify(body)); headers['Content-Type'] = 'application/json'; }
  headers['X-Pdrs-Parity-Case'] = caseId;
  if (token) headers.Authorization = `Bearer ${token}`;
  if (body) headers['Content-Length'] = body.length;
  return new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port: entry, method, path, headers, agent: false }, res => {
      emit('client.jsonl', { kind: 'http_response', case: caseId, status: res.statusCode,
        // rawHeaders is the flat ordered name/value list including duplicates.
        headers: res.rawHeaders, handler: res.headers['x-pilotdeck-handler'] ?? null });
      let offset = 0; const chunks = [];
      res.on('data', chunk => { chunks.push(chunk); emit('client.jsonl', { kind: 'http_chunk', case: caseId,
        offset, bytes: chunk.length, body_b64: chunk.toString('base64') }); offset += chunk.length; onChunk?.(chunk); });
      res.on('error', reject);
      res.on('aborted', () => reject(new Error(`HTTP aborted ${caseId}`)));
      res.on('end', () => {
        const bytes = Buffer.concat(chunks);
        const ok = expected.includes(res.statusCode);
        emit('cases.jsonl', { kind: 'case_http', case: caseId, route: definition.route,
          method, path, status: res.statusCode, handler: res.headers['x-pilotdeck-handler'] ?? null, ok });
        if (!ok) return reject(new Error(`${caseId} status ${res.statusCode}: ${bytes.toString().slice(0, 300)}`));
        let json; try { json = JSON.parse(bytes); } catch { /* binary/SSE evidence remains */ }
        resolve({ status: res.statusCode, bytes, json, headers: res.rawHeaders });
      });
    });
    req.setTimeout(45000, () => req.destroy(new Error(`HTTP timeout ${caseId}`)));
    req.on('error', reject); req.end(body);
  });
}
async function socket(port = entry, path = '/ws', observer = false) {
  const id = ++connection;
  const ws = new WS(`ws://127.0.0.1:${port}${path}?token=${encodeURIComponent(token)}`, { perMessageDeflate: false, headers: { 'X-Pdrs-Parity-Case': observer ? 'activity.out-of-band' : `ws.${currentScenario}.${id}` } });
  const frames = []; let observedClose;
  const file = observer ? 'activity-direct.jsonl' : 'client.jsonl';
  const meta = { connection: id, port, origin: `ws://127.0.0.1:${port}`, out_of_band: observer, user_id: userId };
  const evidence = (value) => emit(file, { ...meta, ...value });
  ws.on('message', (data, binary) => {
    evidence({ kind: 'ws_frame', dir: 'u2c', opcode: binary ? 'binary' : 'text', payload_b64: data.toString('base64') });
    let parsed; try { parsed = JSON.parse(data); } catch (error) { evidence({ kind: 'parse_error', error: String(error) }); return; }
    frames.push(parsed);
  });
  for (const opcode of ['ping', 'pong']) ws.on(opcode, data => evidence({ kind: 'ws_frame', dir: 'u2c', opcode, payload_b64: data.toString('base64') }));
  ws.on('close', (code, reason) => { observedClose = code; sockets.delete(ws); evidence({ kind: 'ws_frame', dir: 'u2c', opcode: 'close', code, reason_b64: reason.toString('base64') }); });
  ws.on('error', error => evidence({ kind: 'ws_error', error: String(error) }));
  ws.on('upgrade', response => evidence({ kind: 'ws_handshake', status: response.statusCode, headers: response.rawHeaders }));
  sockets.add(ws);
  await until(() => ws.readyState === WS.OPEN || ws.readyState === WS.CLOSED, 'WS open');
  assert.equal(ws.readyState, WS.OPEN);
  const send = value => { const raw = JSON.stringify(value); evidence({ kind: 'ws_frame', dir: 'c2u', opcode: 'text', payload_b64: Buffer.from(raw).toString('base64') }); ws.send(raw); };
  const close = async () => { evidence({ kind: 'ws_frame', dir: 'c2u', opcode: 'close', code: 1000 }); ws.close(1000); await until(() => ws.readyState === WS.CLOSED, 'WS close', 8000); assert.equal(observedClose, 1000, 'WS close reply code'); };
  return { ws, frames, send, close, id };
}
async function snapshot(observer, phase) {
  const before = observer.frames.filter(f => f.type === 'session-activity-snapshot').length;
  observer.send({ type: 'get-session-activity' });
  await until(() => observer.frames.filter(f => f.type === 'session-activity-snapshot').length > before, 'direct snapshot');
  const frame = observer.frames.filter(f => f.type === 'session-activity-snapshot').at(-1);
  // Preserve exact per-session revisions. Never subtract a global max revision.
  emit('activity-direct.jsonl', { kind: 'activity_anchor', out_of_band: true, port: direct,
    connection: observer.id, user_id: userId, phase, frame });
  return frame;
}
const session = suffix => `web:33333333-3333-4333-8333-00000000000${suffix}`;
const command = id => ({ type: 'pilotdeck-command', command: 'EXP008_TEXT', options: { projectPath: project, cwd: project, sessionId: id, permissionMode: 'default' } });
let observer;
const results = {};
async function scenario(name, work) {
  currentScenario = name;
  try { await work(); results[name] = { status: 'PASS' }; }
  catch (error) { results[name] = { status: 'FAIL', error: String(error), stack: error.stack }; }
  writeFileSync(join(output, 'scenarios.json'), JSON.stringify(results, null, 2) + '\n');
}
try {
  await scenario('auth', async () => {
    const status = await request('auth.status', '/api/auth/status'); assert(status.json.needsSetup);
    await request('auth.user.unauthorized', '/api/auth/user');
    const credentials = { username: 'exp010', password: 'exp010-isolated-password' };
    const registered = await request('auth.register', '/api/auth/register', { method: 'POST', body: credentials });
    token = registered.json.token; userId = registered.json.user.id; assert(token); assert(userId !== undefined);
    await request('auth.login.invalid', '/api/auth/login', { method: 'POST', body: { ...credentials, password: 'exp010-wrong' } });
    const logged = await request('auth.login', '/api/auth/login', { method: 'POST', body: credentials }); token = logged.json.token; assert(token);
    await request('auth.user', '/api/auth/user');
  });
  assert(token, 'auth must complete before dependent scenarios');
  observer = await socket(direct, '/ws', true);
  await snapshot(observer, 'before-scenarios');
  await scenario('chat', async () => {
    const a = await socket(), b = await socket();
    a.send(command(session(1))); b.send(command(session(2)));
    await until(() => [a,b].every(s => s.frames.some(f => f.kind === 'complete' || f.kind === 'error')), 'two concurrent mock turns');
    for (const s of [a,b]) { assert(!s.frames.some(f => f.kind === 'error')); assert(s.frames.some(f => JSON.stringify(f).includes('EXP008_MOCK_OK'))); }
    const anchor = await snapshot(observer, 'after-concurrent-chat');
    for (const id of [session(1),session(2)]) assert(anchor.activities.some(a => a.sessionId === id && Number.isInteger(a.revision)), `real direct activity ${id}`);
    await a.close(); await b.close();
  });
  await scenario('readonly', async () => {
    await request('readonly.projects', '/api/projects');
    await request('readonly.sessions', '/api/projects/exp008-fixture/sessions');
    await request('readonly.messages', `/api/sessions/${encodeURIComponent(session(1))}/messages?projectPath=${encodeURIComponent(project)}`);
    await request('readonly.config', '/api/config'); await request('readonly.models', '/api/models');
    await request('shadow.files', `/api/projects/files?projectKey=${encodeURIComponent(project)}`);
    // The first registered /:scope handler shadows /read. Capture that actual error.
    await request('shadow.mcp', '/api/mcp/config/read', { expected: [400] });
    await request('shadow.model', `/api/sessions/model?projectKey=${encodeURIComponent(project)}&sessionKey=${encodeURIComponent(session(1))}`);
  });
  await scenario('watch', async () => {
    const watched = await socket(); watched.send({ type: 'watch-session', sessionId: session(1), options: { projectPath: project } });
    await until(() => watched.frames.some(f => f.type === 'input-queue-state'), 'first watch state');
    await watched.close();
    const resumed = await socket(); resumed.send({ type: 'watch-session', sessionId: session(1), options: { projectPath: project } });
    await until(() => resumed.frames.some(f => f.type === 'input-queue-state'), 'reconnected watch state');
    resumed.send({ type: 'get-session-activity' });
    await until(() => resumed.frames.some(f => f.type === 'session-activity-snapshot'), 'reconnected snapshot');
    await snapshot(observer, 'after-watch-reconnect');
    resumed.send({ type: 'unwatch-session', sessionId: session(1) }); await resumed.close();
    const nonce = 'exp010'; const sentinel = `PDSENTINEL_${nonce}`;
    const init = { type: 'init', projectPath: project, sessionId: 'exp010-pty', provider: 'plain-shell', isPlainShell: true, initialCommand: 'exec bash -i', hasSession: false, cols: 80, rows: 24 };
    const shell = await socket(entry, '/shell'); shell.send(init);
    await wait(1000);
    const input = `printf 'PDSENTINEL_%s\\n' '${nonce}'\n`;
    assert(!input.includes(sentinel), 'PTY input echo cannot satisfy sentinel');
    shell.send({ type: 'input', data: input });
    const text = s => s.frames.filter(f => f.type === 'output').map(f => f.data || '').join('');
    await until(() => text(shell).includes(sentinel), 'PTY executed sentinel', 15000);
    await shell.close();
    const shellResumed = await socket(entry, '/shell'); shellResumed.send({ ...init, hasSession: true });
    await wait(300);
    shellResumed.send({ type: 'resize', cols: 100, rows: 30 });
    shellResumed.send({ type: 'input', data: 'stty size\n' });
    await until(() => /30[ \t]+100/.test(text(shellResumed)), 'PTY reconnected resize', 15000);
    shellResumed.send({ type: 'input', data: 'exit\n' });
    await until(() => text(shellResumed).includes('Process exited with code 0'), 'PTY exit', 15000);
    await shellResumed.close();
  });
  await scenario('config', async () => {
    const client = await socket(); const before = client.frames.length;
    await request('config.read', '/api/config');
    const raw = readFileSync(join(home, 'pilotdeck.yaml'), 'utf8') + '\n# exp010 config save\n';
    await request('config.write', '/api/config', { method: 'PUT', body: { raw } });
    await until(() => client.frames.slice(before).some(f => f.type === 'config:reloaded'), 'config broadcast'); await client.close();
  });
  await scenario('upload', async () => {
    await request('upload.limits', '/api/uploads/limits');
    const bytes = Buffer.from('exp010 upload fixture\n');
    const created = await request('upload.create', '/api/uploads', { method: 'POST', expected: [201], body: { projectKey: home, files: [{ clientFileId: 'one', name: 'note.txt', relativePath: 'note.txt', size: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') }] } });
    const id = created.json.uploadId; assert(id);
    // Subscribe first so the recording includes the nonterminal event and completion.
    let firstEvent = '', streamFailure;
    const events = request('upload.events', `/api/uploads/${id}/events`, { onChunk: chunk => { firstEvent += chunk.toString(); } });
    events.catch(error => { streamFailure = error; });
    await until(() => firstEvent.includes('event: upload_started') || streamFailure, 'upload initial SSE event');
    if (streamFailure) throw streamFailure;
    const boundary = 'exp010-boundary';
    const multipart = Buffer.concat([Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="files[one]"; filename="note.txt"\r\nContent-Type: text/plain\r\n\r\n`), bytes, Buffer.from(`\r\n--${boundary}--\r\n`)]);
    await request('upload.content', `/api/uploads/${id}/content`, { method: 'POST', body: multipart, headers: { 'Content-Type': `multipart/form-data; boundary=${boundary}` } });
    const stream = await events; const sse = stream.bytes.toString();
    assert(sse.includes('event: upload_started') && sse.includes('event: upload_completed'), 'upload SSE started + completed');
    assert(sse.indexOf('event: upload_started') < sse.indexOf('event: upload_completed'), 'upload SSE lifecycle order');
    const uploaded = await request('upload.read', `/api/uploads/${id}`); assert.equal(uploaded.json.status, 'completed');
    // Real host is absent: explicitly record its 501 branch, never claim host resume success.
    const jobQuery = new URLSearchParams({ operation: 'job_events', jobId: 'exp010-job' });
    await request('resume.header', `/api/modules/staffdeck-sdk/events?${jobQuery}`, { headers: { 'Last-Event-ID': '2' }, expected: [501] });
    jobQuery.set('lastEventId', '2');
    await request('resume.last-event-query', `/api/modules/staffdeck-sdk/events?${jobQuery}`);
    // Match ui/src/.../public-host-mapping.ts gatewayEvents: preview needs an
    // explicit selection; global job_events above must remain unscoped.
    const previewQuery = new URLSearchParams({ operation: 'preview_job_events', jobId: 'exp010-preview-job' });
    previewQuery.set('scope', 'agent');
    previewQuery.set('agentId', 'exp010-agent');
    previewQuery.set('afterSeq', '2');
    await request('resume.query', `/api/modules/staffdeck-sdk/events?${previewQuery}`, { expected: [501] });
  });
  await snapshot(observer, 'after-scenarios');
  await observer.close();
} catch (error) {
  results.device = { status: 'FAIL', error: String(error), stack: error.stack };
} finally {
  for (const ws of sockets) ws.terminate();
  writeFileSync(join(output, 'scenarios.json'), JSON.stringify(results, null, 2) + '\n');
}
process.exitCode = Object.keys(results).length === 6 && Object.values(results).every(r => r.status === 'PASS') ? 0 : 1;
