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
} else {
  throw new Error(`unknown mode: ${mode}`);
}
