// TS common-policy probe. Rust removes TS read/exec privileges and separately
// verifies Node exec denial under its exact candidate policy.
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import { spawnSync } from 'node:child_process';
import path from 'node:path';

const sentinel = process.argv[2];
// The connect-denial check must test the port the apparatus derives from
// MOCK_PORT (#933 fix-r3): run_round passes it here; no literal in this file.
const port = Number(process.argv[3]);
if (!Number.isInteger(port) || port <= 0 || port > 65535) {
  console.error('usage: isolation_probe.mjs <sentinel> <port>');
  process.exit(1);
}
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
  const socket = net.createConnection({ host: '127.0.0.1', port });
  socket.once('error', error => resolve(['EPERM', 'EACCES'].includes(error.code)));
  socket.once('connect', () => { socket.destroy(); resolve(false); });
  socket.setTimeout(1000, () => { socket.destroy(); resolve(false); });
});
const child = spawnSync(process.execPath, ['--version']);
const home = os.homedir();
let passwdHomeMatches = false;
let passwdLookupDenied = false;
try { passwdHomeMatches = os.userInfo().homedir === home; }
catch { passwdLookupDenied = true; }
const writable = path.join(process.env.TMPDIR, 'probe.txt');
fs.writeFileSync(writable, 'isolated');
const result = {
  osHomedir: home,
  homeMatches: home === process.env.HOME,
  // passwd lookup is not remapped by Seatbelt; access outside runtime is denied.
  passwdHomeMatches,
  passwdLookupDenied,
  outsideReadDenied: denied(() => fs.readFileSync(sentinel)),
  outsideMetadataDenied: denied(() => fs.statSync(sentinel)),
  // This harmless file is present on the calibration host. ENOENT is not a
  // denial: missing fixtures/unsupported hosts block instead of faking proof.
  privateReadDenied: denied(() => fs.readFileSync('/opt/homebrew/etc/openssl@3/private/.keepme')),
  outsideWriteDenied: denied(() => fs.writeFileSync(sentinel, 'unexpected')),
  listenDenied: socketDenied,
  connectDenied,
  childDenied: ['EPERM', 'EACCES'].includes(child.error?.code),
  runtimeWritable: fs.readFileSync(writable, 'utf8') === 'isolated',
  stdinIsTTY: Boolean(process.stdin.isTTY),
};
fs.unlinkSync(writable);
const checks = ['homeMatches', 'outsideReadDenied', 'outsideWriteDenied', 'listenDenied',
  'connectDenied', 'childDenied', 'runtimeWritable', 'outsideMetadataDenied', 'privateReadDenied'];
result.status = checks.every(key => result[key]) && !result.stdinIsTTY ? 'PASS' : 'FAIL';
console.log(JSON.stringify(result));
process.exitCode = result.status === 'PASS' ? 0 : 1;
