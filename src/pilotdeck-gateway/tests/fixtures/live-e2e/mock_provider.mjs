// Local HTTP mock provider for the live-e2e apparatus (#969).
// Follows the gateway-parity mock.mjs shape (fixed local HTTP + request log
// jsonl) but speaks canonical-model-event NDJSON, because both endpoints are
// this fixture: the TS driver's model port is the only client.
//
// Scenarios are selected by the marker inside the last user message:
//   LIVE_E2E_TEXT -> streamed text deltas, then message_end(stop).
//   LIVE_E2E_TOOL -> first call emits a `read` tool call; once the request
//                    contains a tool_result, finish with text.
//   LIVE_E2E_HANG -> a few events, then the connection is held open for the
//                    client to interrupt (cancel-window fixture; the aborted
//                    socket is recorded). The hold is bounded: a client that
//                    never aborts gets a completed stream, so a broken cancel
//                    window fails the apparatus expectations instead of
//                    tripping the driver's 60s watchdog.
//
// Env: LIVE_E2E_MOCK_PORT (fixed port; required), LIVE_E2E_MOCK_LOG (jsonl
// request log path; required). The tool scenario names its read target as the
// whitespace-delimited path directly after the LIVE_E2E_TOOL marker in the
// user prompt, so one shared mock serves per-case sandboxes.
// Never listens on 18789 and never touches ~/.pilotdeck.
import http from 'node:http';
import { appendFileSync } from 'node:fs';

const port = Number(process.env.LIVE_E2E_MOCK_PORT);
const log = process.env.LIVE_E2E_MOCK_LOG;
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('LIVE_E2E_MOCK_PORT required');
if (!log) throw new Error('LIVE_E2E_MOCK_LOG required');

let requests = 0;
let releaseWindow;
const windowReleased = new Promise(resolve => { releaseWindow = resolve; });

function userText(request) {
  const messages = Array.isArray(request?.messages) ? request.messages : [];
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (message?.role !== 'user' || !Array.isArray(message.content)) continue;
    const text = message.content
      .filter((block) => block?.type === 'text')
      .map((block) => block.text)
      .join('');
    if (text) return text;
  }
  return '';
}

function hasToolResult(request) {
  return (request?.messages ?? []).some((message) =>
    (message?.content ?? []).some((block) => block?.type === 'tool_result'));
}

function scenarioFor(request) {
  const text = userText(request);
  if (text.includes('LIVE_E2E_HANG')) return 'hang';
  if (text.includes('LIVE_E2E_TOOL')) return 'tool';
  if (text.includes('LIVE_E2E_TEXT')) return 'text';
  return 'unknown';
}

function toolTargetFor(request) {
  const match = /LIVE_E2E_TOOL\s+(\S+)/.exec(userText(request));
  return match?.[1];
}

const provider = 'live-e2e';
const model = 'mock-model';
const toolCallId = 'call-live-e2e-read';
// How long the hang scenario holds the connection before finishing it on
// its own. Far above the healthy abort latency (milliseconds after the
// first model event) and far below the driver's 60s watchdog.
const HANG_HOLD_MS = 1_000;

function textEvents(request, index) {
  return [
    { type: 'request_started', provider, model },
    { type: 'message_start', role: 'assistant' },
    { type: 'text_delta', text: `hello-${index}` },
    { type: 'text_delta', text: ' live-e2e text stream' },
    { type: 'message_end', finishReason: 'stop' },
  ];
}

function toolCallEvents(request) {
  const target = toolTargetFor(request);
  if (!target) throw new Error('LIVE_E2E_TOOL marker must name a read target path');
  return [
    { type: 'request_started', provider, model },
    { type: 'message_start', role: 'assistant' },
    { type: 'tool_call_start', id: toolCallId, name: 'read' },
    { type: 'tool_call_delta', id: toolCallId, delta: JSON.stringify({ path: target }) },
    { type: 'tool_call_end', toolCall: { id: toolCallId, name: 'read', input: { path: target } } },
    { type: 'message_end', finishReason: 'tool_call' },
  ];
}

function toolDoneEvents() {
  return [
    { type: 'request_started', provider, model },
    { type: 'message_start', role: 'assistant' },
    { type: 'text_delta', text: 'read tool result observed by the mock provider' },
    { type: 'message_end', finishReason: 'stop' },
  ];
}

function hangEvents() {
  return [
    { type: 'request_started', provider, model },
    { type: 'message_start', role: 'assistant' },
    { type: 'text_delta', text: 'hanging mid-stream for cancel' },
  ];
}

const server = http.createServer(async (req, res) => {
  if (req.method === 'POST' && req.url === '/release-window') {
    releaseWindow();
    res.end('released');
    return;
  }
  if (req.method === 'GET' && req.url === '/health') {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('ok');
    return;
  }
  if (req.method !== 'POST' || req.url !== '/model') {
    res.writeHead(404);
    res.end('not found');
    return;
  }
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  let request;
  try {
    const body = JSON.parse(Buffer.concat(chunks).toString());
    request = body?.request ?? body;
  } catch {
    res.writeHead(400);
    res.end('bad request');
    return;
  }
  const index = ++requests;
  const scenario = scenarioFor(request);
  const events =
    scenario === 'hang' ? hangEvents()
    : scenario === 'tool' ? (hasToolResult(request) ? toolDoneEvents() : toolCallEvents(request))
    : scenario === 'text' ? textEvents(request, index)
    : [];
  res.writeHead(200, { 'content-type': 'application/x-ndjson' });
  let sent = 0;
  let finished = false;
  res.on('close', () => {
    // The observable "provider connection was interrupted": the socket went
    // away before the response could complete on its own.
    appendFileSync(log, JSON.stringify({
      index, scenario, connection: finished ? 'completed' : 'aborted', sent,
      request: {
        roles: (request.messages ?? []).map((message) => message?.role ?? '?'),
        userText: userText(request),
      },
    }) + '\n');
  });
  for (const event of events) {
    if (res.destroyed || res.writableEnded) break;
    res.write(JSON.stringify({ event }) + '\n');
    sent++;
    // Explicit window gate: injections end at the first model event; cancel
    // releases only after the client has observed that event without aborting.
    const gateAt = process.env.LIVE_E2E_INJECTION === 'drop' ? 2 : 1;
    if (sent === gateAt && (scenario === 'hang' || ['drop', 'early-final'].includes(process.env.LIVE_E2E_INJECTION))) {
      await Promise.race([windowReleased, new Promise(resolve => {
        const timer = setTimeout(resolve, 10_000);
        res.once('close', () => { clearTimeout(timer); resolve(); });
      })]);
    }
    // A small deterministic pacing keeps mid-stream cancellation observable.
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  if (scenario === 'hang') {
    // Hold the connection open: the client must interrupt it. When nothing
    // interrupts it within the bound, finish the stream so a cancel window
    // that never aborts still ends (and fails its expectations) without
    // waiting for the driver watchdog.
    await new Promise((resolve) => setTimeout(resolve, HANG_HOLD_MS));
    if (res.destroyed || res.writableEnded) return;
    finished = true;
    res.end(JSON.stringify({ done: true }) + '\n');
    return;
  }
  finished = true;
  res.end(JSON.stringify({ done: true }) + '\n');
});

server.listen(port, '127.0.0.1', () => {
  appendFileSync(log, JSON.stringify({ listening: true, port }) + '\n');
});

function shutdown() {
  server.close(() => process.exit(0));
  // Connections still open (a hung cancel window) must not block reaping.
  setTimeout(() => process.exit(0), 1000).unref();
}
process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
