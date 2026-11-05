import net from 'node:net';
import http from 'node:http';
const server = http.createServer((req, res) => { res.writeHead(200, {'content-length':'2'}); res.end('ok'); });
server.on('clientError', (e, s) => { if (s.writable) s.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n'); });
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
  console.log(`${name}\tbytes=${out.length}\t${JSON.stringify(out.subarray(0,40).toString('latin1'))}`);
}
server.close(); process.exit(0);
