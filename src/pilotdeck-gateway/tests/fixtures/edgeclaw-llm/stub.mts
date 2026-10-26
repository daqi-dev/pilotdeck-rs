// Loopback-only scripted HTTP server; every socket belongs to this fixture.
import {createServer} from 'node:http';
import type {Socket} from 'node:net';
export async function stub(steps: any[], now: () => number) {
  const requests: any[] = [];
  const sockets = new Set<Socket>();
  const waiters: Array<() => void> = [];
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    const headers = Object.fromEntries(Object.entries(req.headers).sort(([a], [b]) => a.localeCompare(b)));
    headers.host = '$HOST';
    requests.push({method: req.method, url: req.url, headers, body: Buffer.concat(chunks).toString('utf8'), at: now()});
    waiters.splice(0).forEach(resolve => resolve());
    const step = steps[requests.length - 1] ?? steps.at(-1);
    if (!step) { res.writeHead(599); res.end('script exhausted'); return; }
    if (step.kind === 'disconnect') { req.socket.destroy(); return; }
    if (step.kind === 'headers-hang') return;
    res.writeHead(step.status ?? 200, {'content-type': 'application/json'});
    if (step.kind === 'body-hang') { res.flushHeaders(); return; }
    res.end(step.body ?? '');
  });
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw Error('not TCP');
  return {
    origin: `http://127.0.0.1:${address.port}`, requests,
    async received(n: number) { while (requests.length < n) await new Promise<void>(resolve => waiters.push(resolve)); },
    async close() { for (const socket of sockets) socket.destroy(); await new Promise<void>(resolve => server.close(() => resolve())); },
  };
}
