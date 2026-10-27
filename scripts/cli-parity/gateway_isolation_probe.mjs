#!/usr/bin/env node
// #933 D0-pr2 gateway isolation probe: run under the ts-mock sandbox profile
// (sandbox-exec) and prove the loopback allow rule opened exactly one port.
// argv: [allowed-port, default-gateway-port, other-port, unix-socket-path].
// Exit 0 iff: connectAllowed true AND connectDefaultGatewayDenied /
// connectOtherDenied / connectUnixDenied / listenDenied all EPERM. The user's
// running PilotDeck sits on the default gateway port: kernel-level denial is
// the red line of this batch.
import net from 'node:net';

const allowed = Number(process.argv[2]);
const gateway = Number(process.argv[3]);
const other = Number(process.argv[4]);
const unixPath = process.argv[5];
const report = {};

function connectTcp(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port });
    socket.setTimeout(2000);
    socket.once('connect', () => { socket.destroy(); resolve(true); });
    socket.once('timeout', () => { socket.destroy(); resolve(false); });
    socket.once('error', () => resolve(false));
  });
}

function epermTcp(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port });
    socket.setTimeout(2000);
    socket.once('connect', () => { socket.destroy(); resolve('CONNECTED'); });
    socket.once('timeout', () => { socket.destroy(); resolve('TIMEOUT'); });
    socket.once('error', (error) => resolve(error.code ?? error.message));
  });
}

function epermUnix(path) {
  return new Promise((resolve) => {
    const socket = net.connect(path);
    socket.setTimeout(2000);
    socket.once('connect', () => { socket.destroy(); resolve('CONNECTED'); });
    socket.once('timeout', () => { socket.destroy(); resolve('TIMEOUT'); });
    socket.once('error', (error) => resolve(error.code ?? error.message));
  });
}

function epermListen() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.once('error', (error) => resolve(error.code ?? error.message));
    server.listen(0, '127.0.0.1', () => server.close(() => resolve('LISTENED')));
  });
}

report.connectAllowed = await connectTcp(allowed);
report.connectDefaultGatewayDenied = await epermTcp(gateway);
report.connectOtherDenied = await epermTcp(other);
report.connectUnixDenied = await epermUnix(unixPath);
report.listenDenied = await epermListen();
console.log(JSON.stringify(report));
const denied = (value) => value === 'EPERM' || value === 'EACCES';
process.exit(report.connectAllowed && denied(report.connectDefaultGatewayDenied)
  && denied(report.connectOtherDenied) && denied(report.connectUnixDenied)
  && denied(report.listenDenied) ? 0 : 1);
