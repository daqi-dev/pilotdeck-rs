// Node v22.22.0 raw-socket lifecycle matrix for issue #702.
//
// Plain `node:http` (no options) + raw `node:net` clients. The real TS entry
// `startGatewayUserDialogStoreHttpServer` also calls `createServer(...)` with no
// options (`resources/PilotDeck @cc7c4e6`,
// `src/gateway/user-dialog/HttpGatewayUserDialogStore.ts:232,243`), so these
// bytes are the baseline the Rust wrapper must follow for the states/events
// reachable in this fixture.
//
// Run: node src/pilotdeck-gateway/tests/fixtures/user-dialog-http-store/lifecycle-matrix/probe.mjs out.json
//
// Each cell starts a fresh server and records the exact bytes the client read
// (`hex` for small responses; length + sha256 + first/last 256 bytes for large
// bodies), the terminal event (EOF / reset / timeout) and timing. Nothing is
// hand-written: rerun and compare byte-for-byte.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { connect } from 'node:net';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

assert.ok(process.version.startsWith('v22.'), 'run with Node 22');
const outPath = resolve(process.argv[2] || 'lifecycle-matrix.json');

const sha256 = (buffer) => createHash('sha256').update(buffer).digest('hex');
const clip = (buffer) => ({
  length: buffer.length,
  sha256: sha256(buffer),
  head: buffer.subarray(0, 256).toString('latin1'),
  tail: buffer.subarray(Math.max(0, buffer.length - 256)).toString('latin1'),
});
const small = (buffer) => ({
  length: buffer.length,
  sha256: sha256(buffer),
  hex: buffer.toString('hex'),
  text: buffer.toString('latin1'),
});
const EARLY_401 =
  '{"version":1,"error":{"code":"unauthorized","message":"Unauthorized"}}';
const MISSING_404 =
  '{"version":1,"error":{"code":"not_found","message":"Not Found"}}';

/** Fresh no-options `node:http` server per cell. */
async function startServer() {
  const state = { invoked: 0, closed: false, closeCallbacks: 0 };
  const server = createServer((request, response) => {
    state.invoked += 1;
    const url = new URL(request.url, 'http://127.0.0.1');
    if (url.pathname === '/early') {
      response.writeHead(401, { 'content-type': 'application/json; charset=utf-8' });
      response.end(EARLY_401);
      return;
    }
    if (url.pathname === '/large') {
      const total = 2 * 1024 * 1024;
      const chunk = Buffer.alloc(16 * 1024, 'a');
      const slow = url.searchParams.has('slow');
      let sent = 0;
      response.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
      const pump = () => {
        if (sent >= total) {
          response.end();
          return;
        }
        sent += chunk.length;
        if (slow) {
          response.write(chunk, () => setTimeout(pump, 2));
        } else if (response.write(chunk)) {
          setImmediate(pump);
        } else {
          response.once('drain', pump);
        }
      };
      pump();
      return;
    }
    response.writeHead(404, { 'content-type': 'application/json; charset=utf-8' });
    response.end(MISSING_404);
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  const port = server.address().port;
  return {
    server,
    port,
    origin: `127.0.0.1:${port}`,
    state,
    close: () =>
      new Promise((ok) => {
        if (state.closed) return ok(state.closeCallbacks);
        state.closed = true;
        server.close(() => {
          state.closeCallbacks += 1;
          ok(state.closeCallbacks);
        });
      }),
    destroy: () => server.closeAllConnections?.(),
  };
}

async function connectRaw(port) {
  const socket = connect({ host: '127.0.0.1', port });
  socket.setNoDelay(true);
  await new Promise((ok, bad) => {
    socket.once('connect', ok);
    socket.once('error', bad);
  });
  return socket;
}

/**
 * One persistent reader per socket: a `data` listener is attached immediately
 * and never removed, so bytes cannot fall between a head read and a body read.
 * `waitForData()` returns the next buffered chunk, `untilClosed()` drains until
 * EOF / reset / deadline.
 */
function attachReader(socket) {
  const state = { chunks: [], ended: false, error: null, closed: false };
  const waiters = new Set();
  const notify = () => {
    for (const wake of [...waiters]) wake();
  };
  socket.on('data', (chunk) => {
    state.chunks.push(chunk);
    notify();
  });
  socket.on('end', () => {
    state.ended = true;
    notify();
  });
  socket.on('error', (error) => {
    state.error = error.code ?? error.name;
    notify();
  });
  socket.on('close', () => {
    state.closed = true;
    notify();
  });
  const wait = (deadline) =>
    new Promise((ok) => {
      const wake = () => {
        clearTimeout(timer);
        waiters.delete(wake);
        ok();
      };
      const timer = setTimeout(wake, Math.max(1, deadline - Date.now()));
      waiters.add(wake);
    });
  return {
    take() {
      const bytes = Buffer.concat(state.chunks);
      state.chunks.length = 0;
      return bytes;
    },
    terminal() {
      if (state.error) return `error:${state.error}`;
      if (state.ended || state.closed) return 'eof';
      return null;
    },
    async waitForData(deadlineMs) {
      const deadline = Date.now() + deadlineMs;
      for (;;) {
        if (state.chunks.length > 0) return this.take();
        const terminal = this.terminal();
        if (terminal) return { terminal, bytes: this.take() };
        if (Date.now() >= deadline) return { terminal: 'deadline', bytes: this.take() };
        await wait(deadline);
      }
    },
    async untilClosed(deadlineMs) {
      const deadline = Date.now() + deadlineMs;
      for (;;) {
        const terminal = this.terminal();
        if (terminal) return { bytes: this.take(), terminal };
        if (Date.now() >= deadline) return { bytes: this.take(), terminal: 'deadline' };
        await wait(deadline);
      }
    },
  };
}

const headOf = (bytes) => {
  const end = bytes.indexOf('\r\n\r\n');
  return end < 0 ? bytes : bytes.subarray(0, end + 4);
};
const advertisesKeepAlive = (bytes) =>
  /connection: keep-alive/i.test(headOf(bytes).toString('latin1'));

async function sleepUntil(promise, deadlineMs) {
  return Promise.race([promise, delay(deadlineMs).then(() => 'pending')]);
}

const cells = {};

// ---------------------------------------------------------------------------
// S3 x E1: head already on the wire advertising keep-alive; 2 MiB body still
// flushing; `server.close()` lands mid-body. Node keeps the in-flight
// connection: a second request on the same socket succeeds.
// ---------------------------------------------------------------------------
cells.s3_body_flushing_close_reuses_same_socket = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    socket.write(`GET /large?slow=1 HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const head = await reader.waitForData(3000);
    const headBytes = Buffer.isBuffer(head) ? head : head.bytes;
    const keeping = advertisesKeepAlive(headBytes);
    // Head is out; close() lands while the 2 MiB body is still flushing.
    const closed = close();
    let bodyDelivered = 0;
    let total = headBytes.length;
    const target = headBytes.length + 2 * 1024 * 1024;
    while (total < target) {
      const chunk = await reader.waitForData(3000);
      if (!Buffer.isBuffer(chunk)) break;
      bodyDelivered += chunk.length;
      total += chunk.length;
      await delay(1);
    }
    // Body complete; close() must not have dropped this advertised connection.
    socket.write(`GET /missing HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const follow = await reader.untilClosed(2000);
    const closeOutcome = await sleepUntil(closed, 2000);
    return {
      advertisedKeepAlive: keeping,
      bodyDelivered,
      secondRequest: clip(follow.bytes),
      secondRequestTerminal: follow.terminal,
      closeOutcome,
      note: 'second request travels on the same socket that saw Connection: keep-alive',
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S4 x E2: early 401 written while the peer dribbles a 3 MiB body. Node writes
// the response and drains the unread body (`_dump`); the full 401 survives.
// ---------------------------------------------------------------------------
cells.s4_early_401_slow_sender = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    const length = 3 * 1024 * 1024;
    socket.write(
      `POST /early HTTP/1.1\r\nHost: ${origin}\r\nContent-Length: ${length}\r\n` +
        `Content-Type: application/json\r\n\r\n`,
    );
    let sent = 0;
    const writer = (async () => {
      while (sent < length) {
        if (socket.destroyed || !socket.writable) return 'write-stopped';
        const chunk = Buffer.alloc(1024, 'b');
        sent += chunk.length;
        if (!socket.write(chunk)) {
          await new Promise((ok) => socket.once('drain', ok));
        }
        await delay(2);
      }
      return 'all-sent';
    })();
    const read = await reader.untilClosed(3000);
    const sentAtRead = sent;
    const writerOutcome = await sleepUntil(writer, 3000);
    await sleepUntil(close(), 2000);
    return {
      requestBodyDeclared: length,
      requestBodySentAtRead: sentAtRead,
      writerOutcome,
      response: small(read.bytes),
      terminal: read.terminal,
      note: 'early 401 with an unread body; Node writes the response before draining',
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S4 x E1: early 401 written, request body still unread, then server.close().
// Node's close runs closeIdleConnections(); the connection is still receiving
// a body, so record whether the response and the drain survive.
// ---------------------------------------------------------------------------
cells.s4_early_401_close = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    const length = 3 * 1024 * 1024;
    socket.write(
      `POST /early HTTP/1.1\r\nHost: ${origin}\r\nContent-Length: ${length}\r\n` +
        `Content-Type: application/json\r\n\r\n`,
    );
    socket.write(Buffer.alloc(64 * 1024, 'e'));
    await delay(30);
    const closed = close();
    let sent = 64 * 1024;
    const writer = (async () => {
      while (sent < length) {
        if (socket.destroyed || !socket.writable) return 'write-stopped';
        const chunk = Buffer.alloc(1024, 'e');
        sent += chunk.length;
        if (!socket.write(chunk)) {
          await new Promise((ok) => socket.once('drain', ok));
        }
        await delay(2);
      }
      return 'all-sent';
    })();
    const read = await reader.untilClosed(2000);
    const sentAtRead = sent;
    const writerOutcome = await sleepUntil(writer, 2000);
    const closeOutcome = await sleepUntil(closed, 2000);
    return {
      requestBodyDeclared: length,
      requestBodySentAtRead: sentAtRead,
      response: small(read.bytes),
      terminal: read.terminal,
      writerOutcome,
      closeOutcome,
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S4 x E3: early 401 then the peer FINs its write side with the body unread.
// ---------------------------------------------------------------------------
cells.s4_early_401_client_fin = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    const length = 3 * 1024 * 1024;
    socket.write(
      `POST /early HTTP/1.1\r\nHost: ${origin}\r\nContent-Length: ${length}\r\n` +
        `Content-Type: application/json\r\n\r\n`,
    );
    socket.write(Buffer.alloc(1024, 'c'));
    await delay(10);
    socket.end();
    const read = await reader.untilClosed(2000);
    await sleepUntil(close(), 2000);
    return {
      requestBodyDeclared: length,
      requestBodySent: 1024,
      response: small(read.bytes),
      terminal: read.terminal,
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S4 x E4: early 401 then the peer RSTs before reading. No bytes can arrive.
// ---------------------------------------------------------------------------
cells.s4_early_401_client_rst = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  attachReader(socket);
  try {
    const length = 3 * 1024 * 1024;
    socket.write(
      `POST /early HTTP/1.1\r\nHost: ${origin}\r\nContent-Length: ${length}\r\n` +
        `Content-Type: application/json\r\n\r\n`,
    );
    await delay(20);
    socket.resetAndDestroy();
    await sleepUntil(close(), 1000);
    return {
      requestBodyDeclared: length,
      bytesRead: 0,
      note: 'client RST: by construction no response bytes can be observed',
    };
  } finally {
    destroy();
  }
})();

// ---------------------------------------------------------------------------
// S3 x E3: client FIN after the head, body still flushing. Node flushes the
// complete body to the half-closed peer.
// ---------------------------------------------------------------------------
cells.s3_body_flushing_client_fin = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    socket.write(`GET /large?slow=1 HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const head = await reader.waitForData(3000);
    const headBytes = Buffer.isBuffer(head) ? head : head.bytes;
    socket.end();
    const rest = await reader.untilClosed(3000);
    await sleepUntil(close(), 2000);
    return {
      head: clip(headBytes),
      rest: clip(rest.bytes),
      terminal: rest.terminal,
      note: 'half-close after the head; Node still flushes the complete body',
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S3 x E4: client RST after the head while the body is flushing.
// ---------------------------------------------------------------------------
cells.s3_body_flushing_client_rst = await (async () => {
  const { port, origin, close, destroy } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    socket.write(`GET /large?slow=1 HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const head = await reader.waitForData(3000);
    const headBytes = Buffer.isBuffer(head) ? head : head.bytes;
    socket.resetAndDestroy();
    await sleepUntil(close(), 1000);
    return {
      head: clip(headBytes),
      note: 'RST after the head: the remaining body cannot arrive',
    };
  } finally {
    destroy();
  }
})();

// ---------------------------------------------------------------------------
// S1 x E1: close() while the handler is still running, before any response
// byte. Node still advertises keep-alive and the same socket serves a second
// request (the connection is in-flight, not idle).
// ---------------------------------------------------------------------------
cells.s1_close_in_handler_before_head = await (async () => {
  const state = { invoked: 0 };
  let closeServer = () => {};
  const closePromise = new Promise((ok) => {
    closeServer = () => server.close(() => ok('closed'));
  });
  const server = createServer((request, response) => {
    state.invoked += 1;
    closeServer();
    response.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
    response.end('{"version":1,"result":[]}');
  });
  await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
  const port = server.address().port;
  const origin = `127.0.0.1:${port}`;
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    socket.write(`GET /late HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const first = await reader.waitForData(3000);
    const firstBytes = Buffer.isBuffer(first) ? first : first.bytes;
    const keeping = advertisesKeepAlive(firstBytes);
    socket.write(`GET /missing HTTP/1.1\r\nHost: ${origin}\r\n\r\n`);
    const second = await reader.untilClosed(2000);
    const closeOutcome = await sleepUntil(closePromise, 2000);
    server.closeAllConnections?.();
    return {
      handlerInvocations: state.invoked,
      firstHead: small(headOf(firstBytes)),
      advertisedKeepAlive: keeping,
      secondRequest: clip(second.bytes),
      secondRequestTerminal: second.terminal,
      closeOutcome,
      note: 'close() inside the handler before writeHead; node:http still advertises keep-alive and reuses the socket',
    };
  } finally {
    socket.destroy();
  }
})();

// ---------------------------------------------------------------------------
// S1 x E2: close() while the peer is slowly sending the request body.
// ---------------------------------------------------------------------------
cells.s1_close_while_slow_body = await (async () => {
  const { port, origin, close, destroy, state } = await startServer();
  const socket = await connectRaw(port);
  const reader = attachReader(socket);
  try {
    const length = 1024 * 1024;
    socket.write(
      `POST /large HTTP/1.1\r\nHost: ${origin}\r\nContent-Length: ${length}\r\n` +
        `Content-Type: application/json\r\n\r\n`,
    );
    socket.write(Buffer.alloc(16 * 1024, 'd'));
    const closed = close();
    const read = await reader.untilClosed(2000);
    const closeOutcome = await sleepUntil(closed, 2000);
    return {
      handlerInvocations: state.invoked,
      response: clip(read.bytes),
      terminal: read.terminal,
      closeOutcome,
      note: 'close() before the body completes: node:http invokes the handler at head time and streams the 200 response; the TS baseline itself waits for the body',
    };
  } finally {
    destroy();
    socket.destroy();
  }
})();

const payload = {
  node: process.version,
  platform: process.platform,
  arch: process.arch,
  generatedAt: new Date().toISOString(),
  cells,
};
await writeFile(outPath, JSON.stringify(payload, null, 2) + '\n', { flag: 'w' });
console.log(`wrote ${outPath}`);
