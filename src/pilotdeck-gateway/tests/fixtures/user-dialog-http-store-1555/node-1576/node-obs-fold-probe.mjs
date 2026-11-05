import http from 'node:http';
import net from 'node:net';
const server = http.createServer((req, res) => { res.writeHead(200, {'Content-Type': 'text/plain'}); res.end('ok'); });
const cases = [
  ['obs-fold-short', 'GET / HTTP/1.1\r\nHost: h\r\nX: a\r\n b\r\nConnection: close\r\n\r\n'],
  ['obs-fold-90000-spaces', 'GET / HTTP/1.1\r\nHost: h\r\nX: a\r\n' + ' '.repeat(90000) + 'b\r\nConnection: close\r\n\r\n'],
  ['obs-fold-16400-bytes', 'GET / HTTP/1.1\r\nHost: h\r\nX: a\r\n ' + 'a'.repeat(16400) + '\r\nConnection: close\r\n\r\n'],
  ['leading-ows-90000', 'GET / HTTP/1.1\r\nHost: h\r\nX: ' + ' '.repeat(90000) + 'a\r\nConnection: close\r\n\r\n'],
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
console.log(JSON.stringify({node: process.version, baseline: 'plain http.createServer defaults'}));
for (const [name, payload] of cases) {
  const r = await once(port, Buffer.from(payload, 'latin1'));
  console.log('\n=== ' + name + ' ===');
  // Date makes a 2xx response differ between runs; print the stable projection
  // (size, status line) and keep the full bytes only when the response carries
  // no Date header, so the file can be re-derived byte for byte a week later.
  // `why` (EOF vs ECONNRESET) is deliberately NOT printed: it depends on how the
  // rejected connection is torn down and jitters between runs (the committed
  // file already shows both spellings), which would make a byte-for-byte
  // re-derivation fail for a reason unrelated to the server's behaviour.
  const dated = /(^|\r\n)Date: /.test(r.text);
  console.log(JSON.stringify({bytes: r.bytes,
    head: r.text.split('\r\n')[0], hex: dated ? undefined : r.hex}));
}
server.close();
