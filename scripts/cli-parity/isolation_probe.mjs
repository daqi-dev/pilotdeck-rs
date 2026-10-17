// Runs before product code, under the exact same OS sandbox and environment.
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import { spawnSync } from 'node:child_process';
import path from 'node:path';

const sentinel = process.argv[2];
const denied = fn => {
  try { fn(); return false; }
  catch (error) { return ['EPERM', 'EACCES'].includes(error.code); }
};
const socketDenied = await new Promise(resolve => {
  const server = net.createServer();
  server.once('error', error => resolve(['EPERM', 'EACCES'].includes(error.code)));
  server.listen(0, '127.0.0.1', () => server.close(() => resolve(false)));
});
const connectDenied = await new Promise(resolve => {
  const socket = net.createConnection({ host: '127.0.0.1', port: 28999 });
  socket.once('error', error => resolve(['EPERM', 'EACCES'].includes(error.code)));
  socket.once('connect', () => { socket.destroy(); resolve(false); });
  socket.setTimeout(1000, () => { socket.destroy(); resolve(false); });
});
const child = spawnSync(process.execPath, ['--version']);
const home = os.homedir();
const writable = path.join(process.env.TMPDIR, 'probe.txt');
fs.writeFileSync(writable, 'isolated');
const result = {
  osHomedir: home,
  homeMatches: home === process.env.HOME,
  // passwd lookup is not remapped by Seatbelt; access outside runtime is denied.
  passwdHomeMatches: os.userInfo().homedir === home,
  outsideReadDenied: denied(() => fs.readFileSync(sentinel)),
  outsideWriteDenied: denied(() => fs.writeFileSync(sentinel, 'unexpected')),
  listenDenied: socketDenied,
  connectDenied,
  childDenied: ['EPERM', 'EACCES'].includes(child.error?.code),
  runtimeWritable: fs.readFileSync(writable, 'utf8') === 'isolated',
  stdinIsTTY: Boolean(process.stdin.isTTY),
};
fs.unlinkSync(writable);
const checks = ['homeMatches', 'outsideReadDenied', 'outsideWriteDenied', 'listenDenied',
  'connectDenied', 'childDenied', 'runtimeWritable'];
result.status = checks.every(key => result[key]) && !result.stdinIsTTY ? 'PASS' : 'FAIL';
console.log(JSON.stringify(result));
process.exitCode = result.status === 'PASS' ? 0 : 1;
