import net from 'node:net';
import http from 'node:http';
const server = http.createServer((req, res) => { res.writeHead(200, {'content-length':'2'}); res.end('ok'); });
await new Promise(r => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
const cases = {
  'X: \\r  \\n':      'GET /list HTTP/1.1\r\nHost: h\r\nX: \r  \nConnection: close\r\n\r\n',
  'X:\\r \\r\\n':     'GET /list HTTP/1.1\r\nHost: h\r\nX:\r \r\nConnection: close\r\n\r\n',
  'X: ok (control)':  'GET /list HTTP/1.1\r\nHost: h\r\nX: ok\r\nConnection: close\r\n\r\n',
  'X: <sp>ok':        'GET /list HTTP/1.1\r\nHost: h\r\nX:  ok\r\nConnection: close\r\n\r\n',
};
for (const [name, raw] of Object.entries(cases)) {
  const out = await new Promise((resolve) => {
    const s = net.connect(port, '127.0.0.1'); let buf = Buffer.alloc(0);
    s.on('connect', () => s.write(raw));
    s.on('data', d => buf = Buffer.concat([buf, d]));
    s.on('close', () => resolve(buf));
    s.on('error', () => resolve(buf));
    setTimeout(() => s.destroy(), 3000);
  });
  // See cr-ows-length-probe.mjs: an empty buffer is reported as NO-RESPONSE
  // instead of a blank head that reads like a normal row.
  const cut = out.indexOf('\r\n');
  const head = out.length === 0 ? 'NO-RESPONSE'
    : out.subarray(0, cut === -1 ? out.length : cut).toString('latin1');
  console.log(`${name}\tbytes=${out.length}\thead=${JSON.stringify(head)}`);
}
server.close(); process.exit(0);
