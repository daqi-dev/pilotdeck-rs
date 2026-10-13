// exp008 test instrumentation only. Never imported by the Rust proxy or by any
// baseline file on disk. Two bounded jobs:
//   1. replace native fs.watch (the sandbox rejects it with EMFILE) with shallow
//      stat polling, exactly as exp003 did, when EXP008_POLL_WATCH=1;
//   2. serve a deterministic mock model on the gateway's existing HTTP listener
//      so no extra port or process is needed (EXP008_MOCK=1).
import { appendFileSync } from 'node:fs';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { EventEmitter } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';

process.on('uncaughtExceptionMonitor', (error) => console.error('[exp008 uncaught]', error));
process.on('unhandledRejection', (error) => console.error('[exp008 rejection]', error));

if (process.env.EXP008_POLL_WATCH === '1') {
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
  console.error('[exp008] sandbox fs.watch replaced by shallow stat polling (no recursive reload coverage)');
}

if (process.env.EXP008_MOCK === '1') {
  const mockResponse = (body) => {
    const messages = body.messages ?? [];
    // Deterministic OpenAI-compatible assistant answer. Keep the exact token so
    // drivers can assert the visible text without normalising anything.
    const text = process.env.EXP008_MOCK_TEXT || 'EXP008_MOCK_OK';
    const message = { role: 'assistant', content: text };
    if (process.env.EXP008_MODEL_LOG) appendFileSync(process.env.EXP008_MODEL_LOG,
      JSON.stringify({ model: body.model, response: message }) + '\n');
    if (!body.stream) return Response.json({ id: 'chatcmpl-exp008', object: 'chat.completion',
      model: body.model ?? 'deterministic', choices: [{ index: 0, message, finish_reason: 'stop' }],
      usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 } });
    const chunks = [
      { choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 12, completion_tokens: 4, total_tokens: 16 } },
    ].map((chunk) => `data: ${JSON.stringify({ id: 'chatcmpl-exp008', object: 'chat.completion.chunk', model: body.model ?? 'deterministic', ...chunk })}\n\n`);
    return new Response(chunks.join('') + 'data: [DONE]\n\n', { headers: { 'content-type': 'text/event-stream' } });
  };
  const createServer = http.createServer;
  http.createServer = function (options, listener) {
    if (typeof options === 'function') { listener = options; options = {}; }
    return createServer.call(this, options, function (request, response) {
      if (request.url !== '/exp008/v1/chat/completions') return listener.call(this, request, response);
      void (async () => {
        const parts = [];
        for await (const chunk of request) parts.push(chunk);
        let body = {};
        try { body = JSON.parse(Buffer.concat(parts).toString('utf8') || '{}'); } catch { /* keep {} */ }
        const result = mockResponse(body);
        response.writeHead(result.status, Object.fromEntries(result.headers));
        response.end(Buffer.from(await result.arrayBuffer()));
      })().catch((error) => { response.writeHead(500); response.end(String(error)); });
    });
  };
  syncBuiltinESMExports();
}

// Test-only streaming routes injected onto the UI server's own app. Records the
// incrementality of SSE/NDJSON pass-through without touching the read-only
// baseline. Only mounted when EXP008_UI_TEST_ROUTES=1.
if (process.env.EXP008_UI_TEST_ROUTES === '1') {
  const createServer = http.createServer;
  http.createServer = function (options, listener) {
    if (typeof options === 'function') { listener = options; options = {}; }
    const server = createServer.call(this, options, listener);
    if (listener && typeof listener.get === 'function' && !listener.__exp008Streams) {
      listener.__exp008Streams = true;
      listener.get('/exp008-stream/sse', (request, response) => {
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache',
          'x-accel-buffering': 'no', connection: 'keep-alive' });
        response.write('data: first\n\n');
        setTimeout(() => response.end('data: last\n\n'), 600);
      });
      listener.get('/exp008-stream/ndjson', (request, response) => {
        response.writeHead(200, { 'content-type': 'application/x-ndjson', 'x-accel-buffering': 'no' });
        response.write('{"event":"first"}\n');
        setTimeout(() => response.end('{"event":"last"}\n'), 600);
      });
      // Scenario 7 (SSE resume): mirror the exact resumption inputs the real
      // GET /api/modules/staffdeck-sdk/events stream branch reads
      // (ui/server/routes/modules.js:334-339): the Last-Event-ID request header
      // or ?lastEventId= for job_events, and ?afterSeq= for preview_job_events.
      // The stream echoes where the resume point came from so the comparator can
      // prove the Rust relay forwarded it verbatim, then emits only events after
      // that point (no replay, no loss).
      listener.get('/exp008-stream/resume', (request, response) => {
        const url = new URL(request.url, 'http://127.0.0.1');
        const headerId = request.headers['last-event-id'];
        const lastEventId = url.searchParams.get('lastEventId');
        const afterSeq = url.searchParams.get('afterSeq');
        const source = headerId !== undefined ? 'header'
          : lastEventId !== null ? 'lastEventId' : afterSeq !== null ? 'afterSeq' : 'none';
        const anchor = Number(source === 'header' ? headerId : source === 'lastEventId' ? lastEventId
          : source === 'afterSeq' ? afterSeq : 0);
        const start = Number.isFinite(anchor) ? anchor + 1 : 1;
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache',
          'x-accel-buffering': 'no', connection: 'keep-alive' });
        let seq = start;
        const timer = setInterval(() => {
          response.write(`id: ${seq}\ndata: {"seq":${seq},"resumeSource":"${source}"}\n\n`);
          if (seq >= 5) { clearInterval(timer); response.end(); }
          seq += 1;
        }, 30);
        request.on('close', () => clearInterval(timer));
      });
    }
    return server;
  };
  syncBuiltinESMExports();
  console.error('[exp008] test-only /exp008-stream/{sse,ndjson,resume} routes mounted on the UI app');
}
