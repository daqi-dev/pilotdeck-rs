// Local-only MCP target and HTTP/TLS CONNECT/SOCKS5 proxy. No DNS or external
// connections: even mcp.test is forwarded exclusively to the allocated target.
import fs from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
const material = JSON.parse(fs.readFileSync(new URL('./tls.json', import.meta.url)));
export async function start(spec, dir) {
  const sockets = new Set();
  const log = (file, value) => fs.appendFileSync(`${dir}/${file}.jsonl`, JSON.stringify(value) + '\n');
  const track = server => server.on('connection', s => {
    sockets.add(s); s.on('close', () => sockets.delete(s)); s.on('error', () => {});
  });
  const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let channel;
  const handler = (req, res) => {
    let body = '';
    req.on('data', chunk => body += chunk);
    req.on('end', () => {
      if (req.method === 'GET' && spec.transport === 'sse') {
        channel = res;
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        res.write('event: endpoint\ndata: /message\n\n'); return;
      }
      if (!body) { res.writeHead(405).end(); return; }
      const rpc = JSON.parse(body);
      log('target', { method: rpc.method, proxyAuth: req.headers['proxy-authorization'] ?? null });
      if (!('id' in rpc)) { res.writeHead(202).end(); return; }
      const result = rpc.method === 'initialize'
        ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'local', version: '1' } }
        : { tools: [{ name: 'via_proxy', description: 'local fixture', inputSchema: { type: 'object' } }] };
      const reply = JSON.stringify({ jsonrpc: '2.0', id: rpc.id, result });
      if (spec.transport === 'sse') {
        channel.write(`event: message\ndata: ${reply}\n\n`); res.writeHead(202).end();
      } else { res.writeHead(200, { 'content-type': 'application/json' }).end(reply); }
    });
  };
  const target = spec.targetTls ? https.createServer(material, handler) : http.createServer(handler);
  track(target); await listen(target);
  const forward = (socket, host, port, reply, rest) => {
    if (!['mcp.test', '127.0.0.1', '[::1]'].includes(host) || port !== target.address().port) {
      socket.destroy(); return;
    }
    const upstream = net.connect(target.address().port, '127.0.0.1', () => {
      socket.write(reply); if (rest.length) upstream.write(rest);
      socket.pipe(upstream).pipe(socket);
    });
    sockets.add(upstream);
    upstream.on('error', () => socket.destroy());
    upstream.on('close', () => { sockets.delete(upstream); socket.destroy(); });
    socket.on('close', () => upstream.destroy());
  };
  const accept = socket => {
    socket.on('error', () => {}); socket.setTimeout(5000, () => socket.destroy());
    let buffer = Buffer.alloc(0), state = 'hello', authenticated = false;
    const read = chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (spec.scheme === 'http' || spec.scheme === 'https') {
        const end = buffer.indexOf('\r\n\r\n'); if (end < 0) return;
        const head = buffer.subarray(0, end).toString();
        const match = /^CONNECT (.+):(\d+) HTTP\/1.1/.exec(head);
        if (!match) { socket.destroy(); return; }
        authenticated = head.toLowerCase().includes('proxy-authorization: basic ');
        const entry = { scheme: spec.scheme, host: match[1], auth: authenticated };
        log('proxy', entry); socket.off('data', read);
        if (spec.reject) { socket.end('HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n'); return; }
        if (spec.auth && !head.includes(`Basic ${Buffer.from(spec.auth).toString('base64')}`)) { socket.destroy(); return; }
        forward(socket, match[1], Number(match[2]), 'HTTP/1.1 200 Connection Established\r\n\r\n', buffer.subarray(end + 4));
        return;
      }
      if (state === 'hello') {
        if (buffer.length < 2 || buffer.length < 2 + buffer[1]) return;
        const methods = [...buffer.subarray(2, 2 + buffer[1])];
        log('handshake', { version: buffer[0], methods });
        if (buffer[0] !== 5) { socket.destroy(); return; }
        buffer = buffer.subarray(2 + buffer[1]);
        socket.write(Buffer.from([5, spec.badMethod ? 255 : spec.auth ? 2 : 0]));
        if (spec.badMethod) return;
        state = spec.auth ? 'auth' : 'connect';
      }
      if (state === 'auth') {
        if (buffer.length < 2 || buffer.length < 3 + buffer[1]) return;
        const ulen = buffer[1], plen = buffer[2 + ulen];
        if (buffer.length < 3 + ulen + plen) return;
        if (buffer[0] !== 1) { socket.destroy(); return; }
        const credential = `${buffer.subarray(2, 2 + ulen)}:${buffer.subarray(3 + ulen, 3 + ulen + plen)}`;
        authenticated = credential === spec.auth;
        log('auth', { ok: authenticated });
        buffer = buffer.subarray(3 + ulen + plen);
        socket.write(Buffer.from([1, authenticated ? 0 : 1]));
        if (!authenticated) return;
        state = 'connect';
      }
      if (state === 'connect') {
        if (buffer.length < 5) return;
        const type = buffer[3], size = type === 3 ? buffer[4] : type === 1 ? 4 : 16;
        const offset = type === 3 ? 5 : 4;
        if (buffer.length < offset + size + 2) return;
        const host = type === 3 ? buffer.subarray(offset, offset + size).toString()
          : type === 1 ? [...buffer.subarray(offset, offset + size)].join('.') : '[::1]';
        log('proxy', { scheme: spec.scheme, host, auth: authenticated, version: buffer[0], command: buffer[1], reserved: buffer[2], addressType: type });
        if (buffer[0] !== 5 || buffer[1] !== 1 || buffer[2] !== 0 || ![1, 3, 4].includes(type)) { socket.destroy(); return; }
        socket.off('data', read);
        const response = Buffer.from([5, spec.reject ? 5 : 0, 0, 1, 127, 0, 0, 1, 0, 0]);
        if (spec.reject) { socket.end(response); return; }
        forward(socket, host, buffer.readUInt16BE(offset + size), response, buffer.subarray(offset + size + 2));
      }
    };
    socket.on('data', read);
  };
  const proxy = spec.scheme === 'https' ? tls.createServer(material, accept) : net.createServer(accept);
  track(proxy); proxy.on('connection', () => log('connection', { accepted: true })); await listen(proxy);
  return { target: target.address().port, proxy: proxy.address().port,
    stop() { for (const socket of sockets) socket.destroy(); target.close(); proxy.close(); } };
}
if (process.argv[2] === '--serve') {
  const server = await start(JSON.parse(process.argv[3]), process.argv[4]);
  console.log(JSON.stringify({ target: server.target, proxy: server.proxy }));
  process.stdin.resume(); process.stdin.on('end', () => { server.stop(); process.exit(0); });
}
