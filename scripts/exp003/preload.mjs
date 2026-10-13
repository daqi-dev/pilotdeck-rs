// Test instrumentation only. Never imported by the Rust proxy or baseline files.
// Records at the actual client boundary, including the unmodified UI bridge.
import { appendFileSync } from 'node:fs';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';

process.on('uncaughtExceptionMonitor', (error) => console.error('[exp003 uncaught]', error));
process.on('unhandledRejection', (error) => console.error('[exp003 rejection]', error));

// The macOS managed sandbox rejects native fs.watch with EMFILE, sometimes
// terminating the CLI through its uncaughtException handler. Only this bounded
// fixture opts into shallow stat polling; recursive extension reload is outside A1.
if (process.env.EXP003_POLL_WATCH === '1') {
  fs.watch = function (filename, options, listener) {
    if (typeof options === 'function') { listener = options; options = {}; }
    const watcher = new EventEmitter();
    if (listener) watcher.on('change', listener);
    const changed = (current, previous) => {
      if (current.mtimeMs !== previous.mtimeMs || current.size !== previous.size)
        watcher.emit('change', 'change', path.basename(String(filename)));
    };
    fs.watchFile(filename, { persistent: false, interval: 250 }, changed);
    watcher.close = () => { fs.unwatchFile(filename, changed); watcher.emit('close'); };
    watcher.ref = watcher.unref = () => watcher;
    options?.signal?.addEventListener('abort', () => watcher.close(), { once: true });
    return watcher;
  };
  syncBuiltinESMExports();
  console.error('[exp003] sandbox fs.watch replaced by shallow stat polling (no recursive reload coverage)');
}

let seq = 0;
let connections = 0;
const record = (entry) => {
  if (process.env.EXP003_FRAMES) appendFileSync(process.env.EXP003_FRAMES,
    JSON.stringify({ seq: ++seq, monotonicNs: process.hrtime.bigint().toString(), ...entry }) + '\n');
};
const NativeWebSocket = globalThis.WebSocket;
globalThis.WebSocket = class RecordedWebSocket extends NativeWebSocket {
  constructor(url, protocols) {
    super(url, protocols);
    this.recordId = ++connections;
    this.recordUrl = String(url);
    this.addEventListener('message', (event) => this.record('receive', String(event.data)));
    this.addEventListener('close', (event) => record({ connection: this.recordId, url: this.recordUrl,
      direction: 'receive', close: { code: event.code, reason: event.reason } }));
  }
  record(direction, raw) {
    let frame;
    try { frame = JSON.parse(raw); } catch { frame = raw; }
    record({ connection: this.recordId, url: this.recordUrl, direction, raw, frame });
  }
  send(data) {
    this.record('send', String(data));
    return super.send(data);
  }
};

// Three allowed listener ports are already used by UI/proxy/gateway. A mock
// provider runs on the gateway's existing HTTP server at a test-only route.
// It still exercises the real provider HTTP client; no fourth listener exists.
if (process.env.EXP003_MOCK === '1') {
  const mockResponse = async (url, options = {}) => {
    const body = JSON.parse(options.body ?? '{}');
    const messages = body.messages ?? [];
    const reconnect = JSON.stringify(messages).includes('EXP003_RECONNECT');
    const toolResult = messages.some((message) => message.role === 'tool');
    const question = reconnect && !toolResult;
    const message = question ? { role: 'assistant', content: null, tool_calls: [{
      id: 'call_exp003_question', type: 'function', function: { name: 'ask_user_question',
        arguments: JSON.stringify({ questions: [{ question: 'Continue exp003?', header: 'exp003',
          options: [{ label: 'yes', description: 'Continue' }, { label: 'no', description: 'Stop' }] }] }) },
    }] } : { role: 'assistant', content: 'EXP003_MOCK_OK' };
    const finish = question ? 'tool_calls' : 'stop';
    if (process.env.EXP003_MODEL_LOG) appendFileSync(process.env.EXP003_MODEL_LOG,
      JSON.stringify({ url, request: body, response: message }) + '\n');
    if (!body.stream) return Response.json({ id: 'chatcmpl-exp003', object: 'chat.completion',
      model: 'deterministic', choices: [{ index: 0, message, finish_reason: finish }],
      usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 } });
    const delta = question ? { role: 'assistant', tool_calls: message.tool_calls.map((tool) => ({ index: 0, ...tool })) }
      : { role: 'assistant', content: message.content };
    const chunks = [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: finish }], usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 } },
    ].map((chunk) => `data: ${JSON.stringify({ id: 'chatcmpl-exp003', object: 'chat.completion.chunk', model: 'deterministic', ...chunk })}\n\n`);
    return new Response(chunks.join('') + 'data: [DONE]\n\n', { headers: { 'content-type': 'text/event-stream' } });
  };
  const createServer = http.createServer;
  http.createServer = function (options, listener) {
    if (typeof options === 'function') { listener = options; options = {}; }
    return createServer.call(this, options, function (request, response) {
      if (request.url.startsWith('/exp003/stream/')) {
        const sse = request.url.endsWith('/sse');
        response.writeHead(200, { 'content-type': sse ? 'text/event-stream' : 'application/x-ndjson',
          'x-exp003-test': 'stream', 'connection': 'keep-alive' });
        response.write(sse ? 'data: first\n\n' : '{"event":"first"}\n');
        setTimeout(() => response.end(sse ? 'data: last\n\n' : '{"event":"last"}\n'), 500);
        return;
      }
      if (request.url !== '/exp003/v1/chat/completions') return listener.call(this, request, response);
      void (async () => {
        const parts = [];
        for await (const chunk of request) parts.push(chunk);
        const result = await mockResponse(request.url, { body: Buffer.concat(parts).toString('utf8') });
        response.writeHead(result.status, Object.fromEntries(result.headers));
        response.end(Buffer.from(await result.arrayBuffer()));
      })().catch((error) => { response.writeHead(500); response.end(String(error)); });
    });
  };
  syncBuiltinESMExports();
}
