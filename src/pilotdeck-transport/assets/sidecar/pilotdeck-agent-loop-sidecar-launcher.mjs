import { spawn } from 'node:child_process';

const sidecar = process.env.PILOTDECK_AGENT_LOOP_RUST_SIDECAR_PATH;
if (sidecar === undefined || sidecar.length === 0) {
  console.error('PILOTDECK_AGENT_LOOP_RUST_SIDECAR_PATH is required.');
  process.exit(2);
}

const child = spawn(sidecar, ['--stdio'], {
  env: process.env,
  stdio: 'inherit',
  windowsHide: true,
});
const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
const forward = new Map(signals.map(signal => [signal, () => {
  if (!child.killed) child.kill(signal);
}]));
for (const [signal, handler] of forward) process.on(signal, handler);

let settled = false;
function finish(code, signal) {
  if (settled) return;
  settled = true;
  for (const [name, handler] of forward) process.off(name, handler);
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
}

child.once('error', error => {
  console.error(`Unable to start Rust AgentLoop sidecar: ${error.message}`);
  finish(1);
});
child.once('exit', finish);
