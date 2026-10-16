import http from 'node:http';
import { syncBuiltinESMExports } from 'node:module';
import { appendFileSync } from 'node:fs';

const originalCreateServer = http.createServer;
let serial = 0;

function record(row) {
  appendFileSync(
    process.env.PDRS_UPGRADE_OBSERVATION,
    JSON.stringify({ ...row, pid: process.pid, at: process.hrtime.bigint().toString() }) + '\n',
  );
}

// Observation only: do not catch or replace the baseline's exception policy.
process.on('uncaughtExceptionMonitor', error => {
  record({ event: 'uncaught-exception', message: error.message, stack: error.stack });
});

http.createServer = function (...args) {
  const server = Reflect.apply(originalCreateServer, this, args);
  server.prependListener('request', request => {
    record({ event: 'http-request', method: request.method, url: request.url });
  });
  server.prependListener('upgrade', (request, socket, head) => {
    const id = ++serial;
    record({ event: 'upgrade', id, method: request.method, url: request.url,
      headHex: head.toString('hex'), headBytes: head.length });
    socket.prependListener('data', data => {
      record({ event: 'socket-data', id, hex: Buffer.from(data).toString('hex') });
    });
    socket.on('end', () => record({ event: 'socket-end', id }));
    socket.on('close', hadError => record({ event: 'socket-close', id, hadError }));
    socket.on('error', error => record({ event: 'socket-error', id, message: error.message }));
  });
  return server;
};
syncBuiltinESMExports();
