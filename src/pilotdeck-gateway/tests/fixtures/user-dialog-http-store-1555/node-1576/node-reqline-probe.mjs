import http from 'node:http';
import net from 'node:net';
const server = http.createServer((req, res) => { res.writeHead(200, {'Content-Type': 'text/plain'}); res.end('ok'); });
const cases = [
  ['reqline-90000-spaces-before-target', 'GET' + ' '.repeat(90000) + '/ HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n'],
  ['reqline-90000-spaces-after-target', 'GET /' + ' '.repeat(90000) + 'HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n'],
  ['reqline-short-double-space', 'GET  / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n'],
  ['plain', 'GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n'],
];
function once(port, payload, timeoutMs = 5000) {
  return new Promise((resolve) => {
    const socket = net.connect(port, '127.0.0.1');
    let data = Buffer.alloc(0); let done = false;
    const finish = (why) => { if (done) return; done = true; socket.destroy();
      resolve({why, bytes: data.length, hex: data.toString('hex'), text: data.toString('latin1')}); };
    socket.setTimeout(timeoutMs, () => finish('timeout'));
    socket.on('connect', () => socket.write(payload));
    socket.on('data', (c) => { data = Buffer.concat([data, c]); });
    socket.on('end', () => finish('EOF'));
    socket.on('close', () => finish('closed'));
    socket.on('error', (e) => finish('error:' + e.code));
  });
}
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
console.log(JSON.stringify({node: process.version}));
for (const [name, payload] of cases) {
  const r = await once(port, Buffer.from(payload, 'latin1'));
  // No `why` field: EOF vs ECONNRESET is a teardown-timing detail, not server
  // behaviour, and it jitters between runs (see node-obs-fold-probe.mjs).
  console.log(JSON.stringify({case: name, status: (r.text.match(/^HTTP\/1\.1 (\d{3})/)||[null,null])[1], bytes: r.bytes, head: r.text.split('\r\n')[0]}));
}
server.close();
