// Explicit bounded fixture: shallow polling only; no recursive reload coverage.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { EventEmitter } from 'node:events';
import { syncBuiltinESMExports } from 'node:module';
if (process.env.GP_POLL_WATCH === '1') {
  fs.watch = function(filename, options, listener) {
    if (typeof options === 'function') { listener = options; options = {}; }
    const watcher = new EventEmitter();
    if (listener) watcher.on('change', listener);
    const changed = (a, b) => {
      if (a.mtimeMs !== b.mtimeMs || a.size !== b.size) watcher.emit('change', 'change', path.basename(String(filename)));
    };
    fs.watchFile(filename, { persistent: false, interval: 250 }, changed);
    watcher.close = () => { fs.unwatchFile(filename, changed); watcher.emit('close'); };
    watcher.ref = watcher.unref = () => watcher;
    options?.signal?.addEventListener('abort', () => watcher.close(), { once: true });
    return watcher;
  };
}
// Synthetic streaming transport fixture, clearly outside production /api/web.
// Model mock lives in its own process; the gateway's model HTTP client is real.
const createServer = http.createServer;
http.createServer = function(listener) {
  return createServer.call(this, (req, res) => {
    if (!req.url.startsWith('/__ruler/stream/')) return listener.call(this, req, res);
    const remote = http.request(`${process.env.GP_MOCK_URL}${req.url}`, (reply) => {
      res.writeHead(reply.statusCode, reply.headers); reply.pipe(res);
    });
    remote.on('error', (error) => { res.writeHead(502); res.end(String(error)); });
    req.pipe(remote);
  });
};
syncBuiltinESMExports();
