// Temporary keys are generated locally, never stored in the repository.
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';
import https from 'node:https';
import http from 'node:http';
import net from 'node:net';
import { join } from 'node:path';
export async function start(dir) {
  const file = name => join(dir, name);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(file('openssl.cnf'), '[req]\ndistinguished_name=dn\n[dn]\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n[leaf]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n');
  const openssl = args => execFileSync('openssl', args, { cwd: dir, stdio: 'pipe', timeout: 10000 });
  for (const name of ['ca', 'other']) openssl(['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', `${name}.key`, '-out', `${name}.pem`, '-days', '2', '-subj', `/CN=PDRS temporary ${name}`, '-config', 'openssl.cnf', '-extensions', 'ca']);
  openssl(['req', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'leaf.key', '-out', 'leaf.csr', '-subj', '/CN=PDRS local fixture', '-config', 'openssl.cnf']);
  openssl(['x509', '-req', '-in', 'leaf.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial', '-out', 'leaf.pem', '-days', '2', '-extfile', 'openssl.cnf', '-extensions', 'leaf']);
  const ca = fs.readFileSync(file('ca.pem'), 'utf8');
  fs.writeFileSync(file('empty.pem'), '');
  fs.writeFileSync(file('bad.pem'), '-----BEGIN CERTIFICATE-----\n!invalid!\n-----END CERTIFICATE-----\n');
  fs.writeFileSync(file('bundle.pem'), fs.readFileSync(file('other.pem'), 'utf8') + ca);
  fs.writeFileSync(file('partial.pem'), ca + fs.readFileSync(file('bad.pem'), 'utf8'));
  fs.writeFileSync(file('bad-first.pem'), fs.readFileSync(file('bad.pem'), 'utf8') + ca);
  fs.writeFileSync(file('bad-der.pem'), '-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n' + ca);
  fs.mkdirSync(file('plain-dir')); fs.writeFileSync(file('plain-dir/ca.pem'), ca);
  fs.mkdirSync(file('empty-dir'));
  fs.mkdirSync(file('cert-dir'));
  const hash = openssl(['x509', '-in', 'ca.pem', '-hash', '-noout']).toString().trim();
  fs.writeFileSync(file(`cert-dir/${hash}.0`), ca);
  const material = { key: fs.readFileSync(file('leaf.key')), cert: fs.readFileSync(file('leaf.pem')) };
  const sockets = new Set(), servers = [], requests = [];
  const listen = async (server, host) => {
    servers.push(server);
    server.on('connection', socket => {
      sockets.add(socket); socket.on('close', () => sockets.delete(socket));
      socket.on('error', () => {}); socket.setTimeout(5000, () => socket.destroy());
    });
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, host, resolve); });
    return server.address().port;
  };
  const handler = async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    requests.push({ method: req.method, path: req.url });
    let result;
    if (req.url === '/mcp') {
      if (req.method !== 'POST') { res.writeHead(405).end(); return; }
      const rpc = JSON.parse(body);
      if (!('id' in rpc)) { res.writeHead(202).end(); return; }
      result = { jsonrpc: '2.0', id: rpc.id, result: rpc.method === 'initialize'
        ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'tls', version: '1' } }
        : { tools: [] } };
    } else if (req.url.includes('count_tokens')) result = { input_tokens: 7 };
    else if (req.url === '/hook') result = {};
    else result = { id: 'local', type: 'message', role: 'assistant', model: 'probe', content: [{ type: 'text', text: 'tls-ok' }], stop_reason: 'end_turn', usage: { input_tokens: 7, output_tokens: 1 } };
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result));
  };
  const good = `https://127.0.0.1:${await listen(https.createServer(material, handler), '127.0.0.1')}`;
  const bad = `https://[::1]:${await listen(https.createServer(material, handler), '::1')}`;
  const freshGood = `https://127.0.0.1:${await listen(https.createServer(material, handler), '127.0.0.1')}`;
  const freshBad = `https://[::1]:${await listen(https.createServer(material, handler), '::1')}`;
  fs.writeFileSync(file('initial-ca.pem'), ca);
  const targetPort = await listen(http.createServer(handler), '::1');
  const proxy = async host => {
    const server = https.createServer(material);
    server.on('connect', (req, socket, head) => {
      // A loopback-only proxy; reject every destination except our own target.
      if (req.url !== `[::1]:${targetPort}`) { socket.destroy(); return; }
      requests.push({ method: 'CONNECT', path: '<target>' });
      const upstream = net.connect(targetPort, '::1', () => {
        socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
        if (head.length) upstream.write(head);
        socket.pipe(upstream).pipe(socket);
      });
      sockets.add(upstream); upstream.on('close', () => { sockets.delete(upstream); socket.destroy(); });
      upstream.on('error', () => socket.destroy()); socket.on('close', () => upstream.destroy());
    });
    return `https://${host === '::1' ? '[::1]' : host}:${await listen(server, host)}`;
  };
  return { good, bad, freshGood, freshBad, target: `http://[::1]:${targetPort}`, proxy: await proxy('127.0.0.1'), badProxy: await proxy('::1'), requests,
    stop() { for (const socket of sockets) socket.destroy(); for (const server of servers) server.close(); } };
}
if (process.argv[2] === '--serve') {
  const server = await start(process.argv[3]);
  console.log(JSON.stringify({ good: server.good, bad: server.bad, freshGood: server.freshGood, freshBad: server.freshBad, target: server.target, proxy: server.proxy, badProxy: server.badProxy }));
  process.stdin.resume(); process.stdin.on('end', () => { server.stop(); process.exit(0); });
}
