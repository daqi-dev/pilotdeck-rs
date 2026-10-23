// Scripted HTTP CONNECT proxy shared by the real TS recorder and the Rust
// replay. Plain Node ESM, no dependencies, like `../mcp-client/mock_server.mjs`.
//
// It listens on 127.0.0.1, prints `PORT <n>` on stdout, and tunnels every
// `CONNECT authority` it accepts to that authority. Configuration is read from
// the environment:
//
//   PROXY_AUTH=user:pass   require `Proxy-Authorization: Basic ...` exactly
//   PROXY_STATUS=407       refuse every CONNECT with this status (default 200)
//   PROXY_LOG=<path>       append one JSON line per CONNECT (no credentials)
//
// A refused CONNECT answers `<status> ...` with `proxy-authenticate: Basic
// realm="pdrs-mcp-proxy"` for 407 and closes. Every tunnel is closed on
// stdin end / SIGTERM, so `kill_on_drop` in Rust and the TS recorder both
// leave no listener behind.
import fs from 'node:fs';
import net from 'node:net';

const expectedAuth = process.env.PROXY_AUTH === undefined
  ? undefined
  : `Basic ${Buffer.from(process.env.PROXY_AUTH).toString('base64')}`;
const status = Number(process.env.PROXY_STATUS ?? 200);
const logPath = process.env.PROXY_LOG;

function log(entry) {
  if (!logPath) return;
  fs.appendFileSync(logPath, `${JSON.stringify(entry)}\n`);
}

const server = net.createServer((socket) => {
  let buffer = '';
  let settled = false;
  socket.on('error', () => undefined);
  socket.setTimeout(10_000, () => socket.destroy());
  socket.on('data', (chunk) => {
    if (settled) return;
    buffer += chunk.toString('latin1');
    const headEnd = buffer.indexOf('\r\n\r\n');
    if (headEnd === -1) return;
    settled = true;
    socket.setTimeout(0);
    const head = buffer.slice(0, headEnd);
    const lines = head.split('\r\n');
    const [method, target] = (lines.shift() ?? '').split(' ');
    const headers = {};
    for (const line of lines) {
      const at = line.indexOf(':');
      if (at === -1) continue;
      headers[line.slice(0, at).toLowerCase()] = line.slice(at + 1).trim();
    }
    log({
      method,
      target,
      host: headers.host ?? null,
      proxyConnection: headers['proxy-connection'] ?? null,
      // The oracle records whether a Basic credential was sent, never its
      // value; the value is a test-only throwaway.
      proxyAuthorization: headers['proxy-authorization']?.startsWith('Basic ')
        ? 'Basic <scrub>'
        : null,
    });
    if (status !== 200) {
      const reason = status === 407 ? 'Proxy Authentication Required' : 'Forbidden';
      socket.end(
        `HTTP/1.1 ${status} ${reason}\r\n` +
        (status === 407 ? 'proxy-authenticate: Basic realm="pdrs-mcp-proxy"\r\n' : '') +
        'connection: close\r\ncontent-length: 0\r\n\r\n',
      );
      return;
    }
    if (expectedAuth !== undefined && headers['proxy-authorization'] !== expectedAuth) {
      socket.end(
        'HTTP/1.1 407 Proxy Authentication Required\r\n' +
        'proxy-authenticate: Basic realm="pdrs-mcp-proxy"\r\n' +
        'connection: close\r\ncontent-length: 0\r\n\r\n',
      );
      return;
    }
    const separator = target.lastIndexOf(':');
    const host = target.slice(0, separator);
    const port = Number(target.slice(separator + 1));
    const upstream = net.connect(port, host, () => {
      socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      const rest = buffer.slice(headEnd + 4);
      if (rest) upstream.write(Buffer.from(rest, 'latin1'));
      socket.pipe(upstream).pipe(socket);
    });
    upstream.on('error', () => socket.destroy());
    socket.on('close', () => upstream.destroy());
    upstream.on('close', () => socket.destroy());
  });
});
server.listen(0, '127.0.0.1', () => {
  process.stdout.write(`PORT ${server.address().port}\n`);
});
function stop() {
  server.close(() => process.exit(0));
  for (const socket of sockets) socket.destroy();
  setTimeout(() => process.exit(0), 2_000).unref();
}
const sockets = new Set();
server.on('connection', (socket) => {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
});
process.stdin.on('end', stop);
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
