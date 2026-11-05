import net from 'node:net';
import http from 'node:http';
const server = http.createServer((req, res) => { res.writeHead(200, {'content-length':'2'}); res.end('ok'); });
server.on('clientError', (e, s) => { if (s.writable) s.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n'); });
await new Promise(r => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
const pad = (n) => ' '.repeat(n);
const cases = {
  'cr-short':            'GET /list HTTP/1.1\r\nHost: h\r\nX: \r  \nConnection: close\r\n\r\n',
  'cr-90000':            'GET /list HTTP/1.1\r\nHost: h\r\nX: \r' + pad(90000) + '  \nConnection: close\r\n\r\n',
  'cr-pad-16300':        'GET /list HTTP/1.1\r\nHost: h\r\nX: \r' + pad(16300) + '\r\nConnection: close\r\n\r\n',
  'cr-pad-16360':        'GET /list HTTP/1.1\r\nHost: h\r\nX: \r' + pad(16360) + '\r\nConnection: close\r\n\r\n',
  'cr-pad-16380':        'GET /list HTTP/1.1\r\nHost: h\r\nX: \r' + pad(16380) + '\r\nConnection: close\r\n\r\n',
  'cr-pad-16400':        'GET /list HTTP/1.1\r\nHost: h\r\nX: \r' + pad(16400) + '\r\nConnection: close\r\n\r\n',
  'ows-90000-control':   'GET /list HTTP/1.1\r\nHost: h\r\nX: ' + pad(90000) + 'ok\r\nConnection: close\r\n\r\n',
};
for (const [name, raw] of Object.entries(cases)) {
  const out = await new Promise((resolve) => {
    const s = net.connect(port, '127.0.0.1'); let buf = Buffer.alloc(0);
    s.on('connect', () => s.write(raw));
    s.on('data', d => buf = Buffer.concat([buf, d]));
    s.on('close', () => resolve(buf));
    s.on('error', () => resolve(buf));
    setTimeout(() => s.destroy(), 4000);
  });
  console.log(`${name}\treq_len=${Buffer.byteLength(raw)}\tbytes=${out.length}\t${JSON.stringify(out.subarray(0,47).toString('latin1'))}`);
}
server.close(); process.exit(0);
