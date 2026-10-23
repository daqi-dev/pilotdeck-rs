// Scripted stdio MCP server shared by the TS generator and the Rust replay.
// Plain Node ESM with no dependencies, so the Rust test can run it with any
// Node on PATH.
//
// MOCK_DIR holds `script.json` and receives one log per spawned process:
// each process claims the next index with an exclusive create, then appends
// JSON lines to `log-<index>.jsonl`:
//   {"event":"spawn","argv":[...],"cwd":"<rel>","env":{...}}
//   {"event":"frame","line":"<exact bytes received, without the newline>"}
//   {"event":"stdin-end"} / {"event":"exit"}
// `env` lists only variables outside the SDK's default inherited set, so it
// shows exactly what the spec contributed and that nothing else leaked.
//
// script.json: {"procs":[{"<method or tools/call:<name>>":[step...]}]}; process
// i uses procs[min(i, last)]. Steps run in order per received request:
//   {"send": value}   JSON.stringify(value) with every "$id" string -> request id
//   {"line": "text"}  raw line, "$ID" replaced with JSON.stringify(id)
//   {"sleep": ms}     wait
//   {"wait_file": n}  wait until MOCK_DIR/<n> exists (a gate the Rust tests
//                     open; the TS recorder never uses it)
//   {"exit": code}    process.exit(code)
// A request without a handler is left unanswered (hang). The "stdin-end"
// key (Rust tests only) runs its steps after stdin ends, before exiting.
import fs from 'node:fs';
import path from 'node:path';

// ---------------------------------------------------------------------------
// HTTP mode (`--http`): one server per scenario, started by the recorder. It
// prints `PORT <n>` once listening and serves MOCK_DIR/script.json `http`:
//   { "<key>": [response, ...] }  -- the n-th request with that key gets the
//   n-th response (the last one repeats). Keys: "GET <path>", or for a POST
//   the JSON-RPC method (`tools/call:<name>` for calls, `reply` for a client
//   response). Unscripted: GET -> 405, notifications/replies -> 202, other
//   requests -> 404 "no handler".
// Responses: { status?, headers?, json? | text? | sse?: [block...], hold?,
//   channel?, push?: [block...] }. `sse` blocks are written verbatim as one
//   chunk; `hold` keeps the stream open; `channel` stores it for `push` (the
//   legacy SSE transport answers POSTs on its GET stream). Every "$id"
//   string and "$ID" text is replaced by the request id.
// Every request is appended to http.jsonl with its method, path, the
// protocol headers and the exact body.
// ---------------------------------------------------------------------------
async function serveHttp(dir) {
  // `MOCK_TLS` names a JSON file with the shared throwaway fixture key/cert
  // (`tests/fixtures/model-stream/tls.json`); plain HTTP otherwise.
  const tls = process.env.MOCK_TLS
    ? JSON.parse(fs.readFileSync(process.env.MOCK_TLS, 'utf8'))
    : undefined;
  const http = await import(tls ? 'node:https' : 'node:http');
  const script = JSON.parse(fs.readFileSync(path.join(dir, 'script.json'), 'utf8')).http;
  const counters = new Map();
  let channel = null;
  // `proxy-authorization` is logged so the proxy-credential leak regression
  // can assert the header never reaches the tunnelled target.
  const LOGGED = ['accept', 'authorization', 'content-type', 'last-event-id', 'mcp-protocol-version', 'mcp-session-id', 'proxy-authorization', 'x-api-key'];
  const take = (key) => {
    const list = script[key];
    if (!list) return undefined;
    const n = counters.get(key) ?? 0;
    counters.set(key, n + 1);
    return list[Math.min(n, list.length - 1)];
  };
  const fill = (value, id) => {
    if (value === '$id') return id;
    if (typeof value === 'string') return value.split('$ID').join(JSON.stringify(id));
    if (Array.isArray(value)) return value.map((item) => fill(item, id));
    if (value && typeof value === 'object') {
      const out = {};
      for (const key of Object.keys(value)) out[key] = fill(value[key], id);
      return out;
    }
    return value;
  };
  const handler = (req, res) => {
    let body = '';
    req.setEncoding('utf8');
    req.on('data', (chunk) => { body += chunk; });
    req.on('end', () => {
      const headers = {};
      for (const name of LOGGED) if (req.headers[name] !== undefined) headers[name] = req.headers[name];
      fs.appendFileSync(path.join(dir, 'http.jsonl'),
        JSON.stringify({ method: req.method, path: req.url, headers, body }) + '\n');
      let key = `${req.method} ${req.url.split('?')[0]}`;
      let id;
      let isRequest = false;
      if (req.method === 'POST') {
        try {
          const message = JSON.parse(body);
          id = message.id;
          isRequest = message.method !== undefined && message.id !== undefined;
          key = message.method === undefined ? 'reply'
            : message.method === 'tools/call' ? `tools/call:${message.params?.name}` : message.method;
        } catch {
          key = 'invalid';
        }
      }
      let response = take(key);
      if (!response) {
        response = req.method === 'GET' ? { status: 405 }
          : isRequest ? { status: 404, text: 'no handler' } : { status: 202 };
      }
      response = fill(response, id);
      if (response.push && channel) channel.write(response.push.join(''));
      const headersOut = { ...(response.headers ?? {}) };
      if (response.json !== undefined) headersOut['content-type'] ??= 'application/json';
      if (response.sse) headersOut['content-type'] ??= 'text/event-stream';
      res.writeHead(response.status ?? 200, headersOut);
      if (response.sse) {
        res.write(response.sse.join(''));
        if (response.channel) channel = res;
        if (!response.hold && !response.channel) res.end();
        return;
      }
      if (response.json !== undefined) res.end(JSON.stringify(response.json));
      else res.end(response.text ?? '');
    });
  };
  const server = tls ? http.createServer(tls, handler) : http.createServer(handler);
  // Held streams must not keep the recorder waiting on close.
  // MOCK_HOST lets the proxy fixture put the target on 0.0.0.0, which the
  // baseline's proxy installer does not add to NO_PROXY (127.0.0.1 and
  // localhost always are); other fixtures keep the loopback default.
  server.listen(0, process.env.MOCK_HOST || '127.0.0.1', () => {
    process.stdout.write(`PORT ${server.address().port}\n`);
  });
  process.stdin.on('end', () => process.exit(0));
  process.stdin.resume();
}

if (process.argv[2] === '--http') {
  await serveHttp(process.env.MOCK_DIR);
} else {
  const dir = process.env.MOCK_DIR;
  const script = JSON.parse(fs.readFileSync(path.join(dir, 'script.json'), 'utf8'));
  let index = 0;
  for (;; index += 1) {
    try {
      fs.closeSync(fs.openSync(path.join(dir, `proc-${index}.claim`), 'wx'));
      break;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
  }
  const logPath = path.join(dir, `log-${index}.jsonl`);
  const log = (entry) => {
    try {
      fs.appendFileSync(logPath, JSON.stringify(entry) + '\n');
    } catch {
      // The run directory is gone: an orphan outlived its recorder.
    }
  };
  const DEFAULT_ENV = new Set(['HOME', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'USER']);
  const env = {};
  for (const key of Object.keys(process.env).sort()) {
    // macOS injects __CF_USER_TEXT_ENCODING into every process; it is not part
    // of what the client passed and does not exist on Linux.
    if (!DEFAULT_ENV.has(key) && key !== 'MOCK_DIR' && key !== '__CF_USER_TEXT_ENCODING') {
      env[key] = process.env[key];
    }
  }
  const cwd = path.relative(fs.realpathSync(dir), process.cwd());
  log({
    event: 'spawn',
    argv: process.argv.slice(2),
    // An inherited cwd is the caller's and differs between the TS and Rust runs.
    cwd: cwd === '..' || cwd.startsWith('..' + path.sep) || path.isAbsolute(cwd) ? '<outside>' : cwd,
    env,
  });
  process.on('exit', () => log({ event: 'exit' }));
  const procs = script.procs;
  const handlers = procs[Math.min(index, procs.length - 1)];

  function substitute(value, id) {
    if (value === '$id') return id;
    if (Array.isArray(value)) return value.map((item) => substitute(item, id));
    if (value && typeof value === 'object') {
      const out = {};
      for (const key of Object.keys(value)) out[key] = substitute(value[key], id);
      return out;
    }
    return value;
  }

  let queue = Promise.resolve();
  // Consecutive output steps go out in one write, so the client sees them as
  // one stdout chunk on every run (chunking decides JS microtask ordering).
  async function run(steps, id) {
    let pending = '';
    const flush = () => {
      if (pending) process.stdout.write(pending);
      pending = '';
    };
    for (const step of steps) {
      if ('send' in step) pending += JSON.stringify(substitute(step.send, id)) + '\n';
      else if ('line' in step) pending += step.line.split('$ID').join(JSON.stringify(id)) + '\n';
      else if ('sleep' in step) {
        flush();
        await new Promise((resolve) => setTimeout(resolve, step.sleep));
      } else if ('wait_file' in step) {
        flush();
        const gate = path.join(dir, step.wait_file);
        while (!fs.existsSync(gate)) await new Promise((resolve) => setTimeout(resolve, 5));
      } else if ('exit' in step) {
        flush();
        process.exit(step.exit);
      }
    }
    flush();
  }

  let buffer = '';
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    buffer += chunk;
    for (let at = buffer.indexOf('\n'); at !== -1; at = buffer.indexOf('\n')) {
      const line = buffer.slice(0, at);
      buffer = buffer.slice(at + 1);
      log({ event: 'frame', line });
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue;
      }
      const key = message.method === 'tools/call' ? `tools/call:${message.params?.name}` : message.method;
      const steps = key === undefined ? undefined : handlers[key];
      if (steps) queue = queue.then(() => run(steps, message.id));
    }
  });
  process.stdin.on('end', () => {
    log({ event: 'stdin-end' });
    queue.then(() => run(handlers['stdin-end'] ?? [], null)).then(() => process.exit(0));
  });
}
