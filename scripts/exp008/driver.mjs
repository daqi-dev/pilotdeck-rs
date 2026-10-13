// exp008 / B1 WebSocket driver. Talks to the browser API entry (Rust 13001) and,
// for the direct-vs-proxy control, to the TS ui/server (13002). Application
// frames are never rewritten; only collected.
//
//   node driver.mjs <mode> <out.json> <baseline> <project> <port>
//
// modes: chat | chat-raw
// env:   EXP008_TOKEN  (JWT; empty when local auth is disabled)
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import crypto from 'node:crypto';
import http from 'node:http';

const [mode, output, baseline, project, port] = process.argv.slice(2);
const token = process.env.EXP008_TOKEN || '';
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const save = (value) => writeFileSync(output, JSON.stringify(value, null, 2) + '\n');

async function deadline(test, label, ms = 30000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { const result = test(); if (result) return result; await sleep(50); }
  throw new Error(`timeout: ${label}`);
}

function connect(path) {
  const suffix = token ? `?token=${encodeURIComponent(token)}` : '';
  return new WebSocket(`ws://127.0.0.1:${port}${path}${suffix}`);
}

async function openSocket(path) {
  const ws = connect(path);
  const frames = [];
  ws.addEventListener('message', (event) => frames.push(JSON.parse(event.data)));
  await deadline(() => ws.readyState === WebSocket.OPEN, `socket ${path} open`);
  return { ws, frames };
}

const command = (id, text) => ({ type: 'pilotdeck-command', command: text,
  options: { projectPath: project, cwd: project, sessionId: id, permissionMode: 'default' } });
const resolveSession = (fallback) => process.env.EXP008_SESSION || fallback;

if (mode === 'chat') {
  const { ws, frames } = await openSocket('/ws');
  ws.send(JSON.stringify(command(resolveSession('web:exp008-chat'), 'EXP008_TEXT')));
  await deadline(() => frames.some((frame) => frame.kind === 'complete' || frame.kind === 'error'), 'chat complete');
  assert(!frames.some((frame) => frame.kind === 'error'), `no UI error: ${JSON.stringify(frames.find((f) => f.kind === 'error'))}`);
  assert(frames.some((frame) => JSON.stringify(frame).includes('EXP008_MOCK_OK')), 'visible assistant text');
  save({ port, frames });
  ws.close();
} else if (mode === 'chat-raw') {
  // Same command, but keep every raw frame string (both directions) so two runs
  // can be compared.
  const suffix = token ? `?token=${encodeURIComponent(token)}` : '';
  const ws = new WebSocket(`ws://127.0.0.1:${port}/ws${suffix}`);
  const raw = [];
  const sent = [];
  ws.addEventListener('message', (event) => raw.push(String(event.data)));
  await deadline(() => ws.readyState === WebSocket.OPEN, 'raw socket open');
  const frame = JSON.stringify(command(resolveSession('web:exp008-chat-raw'), 'EXP008_TEXT'));
  sent.push(frame);
  ws.send(frame);
  await deadline(() => raw.some((line) => { try { const f = JSON.parse(line); return f.kind === 'complete' || f.kind === 'error'; } catch { return false; } }), 'chat-raw complete');
  save({ port, sent, raw });
  ws.close();
} else if (mode === 'ws-handshake') {
  // Raw HTTP upgrade so an Authorization header can be attached: the baseline
  // accepts it for /ws (ui/server/index.js:490-496) while Node's global
  // WebSocket cannot set request headers. run.py supplies a fixed
  // Sec-WebSocket-Key so Sec-WebSocket-Accept is comparable across two probes.
  const key = process.env.EXP008_WS_KEY || crypto.randomBytes(16).toString('base64');
  const headers = {
    Connection: 'Upgrade',
    Upgrade: 'websocket',
    'Sec-WebSocket-Version': '13',
    'Sec-WebSocket-Key': key,
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  if (process.env.EXP008_WS_PROTOCOL) headers['Sec-WebSocket-Protocol'] = process.env.EXP008_WS_PROTOCOL;
  const result = await new Promise((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port: Number(port), path: '/ws', agent: false, headers });
    req.on('upgrade', (res, socket) => {
      socket.destroy();
      resolve({ status: res.statusCode, headers: res.headers, key, upgraded: true });
    });
    req.on('response', (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, key,
        upgraded: false, body: Buffer.concat(chunks).toString('utf8') }));
    });
    req.on('error', reject);
    req.end();
  });
  save(result);
} else if (mode === 'ws-close') {
  // Capture the close handshake in both directions: the code/reason we send and
  // the code/reason the peer replies with (before this driver saved and closed
  // without observing the reply).
  const code = Number(process.env.EXP008_CLOSE_CODE || 4001);
  const reason = process.env.EXP008_CLOSE_REASON || 'exp008-close';
  const { ws } = await openSocket('/ws');
  const observed = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve({ code: null, reason: null, clean: false, timeout: true }), 8000);
    ws.addEventListener('close', (event) => {
      clearTimeout(timer);
      resolve({ code: event.code, reason: event.reason, clean: event.wasClean });
    });
    ws.close(code, reason);
  });
  save({ port, sentCode: code, sentReason: reason, ...observed });
} else {
  throw new Error(`unknown mode: ${mode}`);
}
