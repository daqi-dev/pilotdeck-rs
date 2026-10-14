import http from 'node:http';
import { readFile, realpath, stat } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { root, requireUI, ports, oldDist, newDist } from './env.mjs';
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2' };
export async function assetPath(frontend, pathname) {
  const dist = frontend === 'old' ? oldDist : newDist;
  let directory = dist, relative = decodeURIComponent(pathname).replace(/^\/+/, '');
  if (frontend === 'new') {
    if (pathname.startsWith('/shared/')) { directory = resolve(root, 'ui-rs/shared'); relative = decodeURIComponent(pathname.slice(8)); }
    else if (pathname === '/vendor/xterm.js') return requireUI.resolve('@xterm/xterm');
    else if (pathname === '/vendor/xterm.css') return requireUI.resolve('@xterm/xterm/css/xterm.css');
    else if (pathname.startsWith('/probe/leptos/')) relative = decodeURIComponent(pathname.slice('/probe/leptos/'.length));
  }
  let file = resolve(directory, relative || 'index.html');
  if (!file.startsWith(directory + sep)) throw new Error('Path escapes asset directory');
  try { if (!(await stat(file)).isFile()) throw new Error('not file'); }
  catch { if (directory !== dist || extname(relative)) throw new Error('Missing asset'); file = resolve(dist, 'index.html'); }
  const [realFile, realDirectory] = await Promise.all([realpath(file), realpath(directory)]);
  if (!realFile.startsWith(realDirectory + sep)) throw new Error('Asset symlink escapes directory');
  return realFile;
}
export async function startFrontend(frontend) {
  if (!['old', 'new'].includes(frontend)) throw new Error('Unknown frontend');
  const sockets = new Set(), upstreamSockets = new Set();
  const server = http.createServer(async (req, res) => {
    const pathname = new URL(req.url, `http://127.0.0.1:${ports[frontend]}`).pathname;
    if (pathname.startsWith('/api/') || pathname.startsWith('/__parity/')) {
      const proxy = http.request({ hostname: '127.0.0.1', port: ports.mock, path: req.url, method: req.method, headers: req.headers }, reply => {
        res.writeHead(reply.statusCode, reply.headers); reply.pipe(res);
      });
      proxy.on('error', () => { if (!res.headersSent) res.writeHead(502); res.end(); });
      req.on('aborted', () => proxy.destroy()); res.on('close', () => { if (!res.writableFinished) proxy.destroy(); });
      req.pipe(proxy); return;
    }
    try { const path = await assetPath(frontend, pathname); const data = await readFile(path); res.writeHead(200, { 'Content-Type': mime[extname(path)] || 'application/octet-stream' }); res.end(data); }
    catch { res.writeHead(404); res.end('Missing or unsafe static asset'); }
  });
  server.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  server.on('upgrade', (req, socket, head) => {
    const pathname = new URL(req.url, `http://127.0.0.1:${ports[frontend]}`).pathname;
    if (!['/ws', '/shell'].includes(pathname)) { socket.destroy(); return; }
    const proxy = http.request({ hostname: '127.0.0.1', port: ports.mock, path: req.url, headers: req.headers });
    socket.on('error', () => proxy.destroy());
    socket.on('close', () => proxy.destroy());
    proxy.on('upgrade', (reply, upstream, upstreamHead) => {
      upstreamSockets.add(upstream); upstream.on('close', () => upstreamSockets.delete(upstream));
      socket.write(`HTTP/1.1 ${reply.statusCode} ${reply.statusMessage}\r\n` + Object.entries(reply.headers).map(([name, value]) => `${name}: ${value}\r\n`).join('') + '\r\n');
      if (upstreamHead.length) socket.write(upstreamHead);
      if (head.length) upstream.write(head);
      socket.on('error', () => upstream.destroy()); upstream.on('error', () => socket.destroy());
      socket.on('close', () => upstream.destroy()); upstream.on('close', () => socket.destroy());
      socket.pipe(upstream).pipe(socket);
    });
    proxy.on('response', reply => { socket.end(`HTTP/1.1 ${reply.statusCode} ${reply.statusMessage}\r\nConnection: close\r\n\r\n`); reply.resume(); });
    proxy.on('error', () => socket.destroy()); proxy.end();
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(ports[frontend], '127.0.0.1', resolve); });
  return { close: async () => { for (const socket of [...sockets, ...upstreamSockets]) socket.destroy(); await new Promise(resolve => server.close(resolve)); } };
}
