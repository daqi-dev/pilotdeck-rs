// The HTTP mock also runs in a service HOME, never in the coordinator's HOME.
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { root } from './env.mjs';
import { assertRealExecutionEnv } from './execution-env.mjs';
import { startMockLLM } from './mock-llm.mjs';

export async function startIsolatedMockLLM(env, children, spawnImpl = spawn) {
  assertRealExecutionEnv(env);
  const child = spawnImpl(process.execPath, [resolve(root, 'scripts/fe-parity/real-mock.mjs')], {
    cwd: root, detached: true, env: { ...env, FE_REAL_RUNTIME: '1', FE_NETWORK_LOG: resolve(env.FE_OUTPUT, 'network.jsonl'),
      NODE_OPTIONS: `--import=${resolve(root, 'scripts/fe-parity/real-network.mjs')}` }, stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
  });
  children.add(child);
  child.feServiceHome = env.HOME;
  const stats = { requests: 0, rejected: 0, errors: [] };
  let readyResolve, readyReject, closed = false, requestedClose = false, receivedStats = false;
  const ready = new Promise((done, reject) => { readyResolve = done; readyReject = reject; });
  const exited = new Promise(done => child.once('close', code => {
    closed = true; children.delete(child);
    if (!requestedClose) stats.errors.push('Isolated mock exited unexpectedly');
    if (requestedClose && !receivedStats) stats.errors.push('Isolated mock exited without final stats');
    if (code !== 0) stats.errors.push(`Isolated mock exited ${code}`);
    readyReject(new Error(`Isolated mock exited before readiness (${code})`)); done();
  }));
  child.stdout?.resume(); child.stderr?.resume();
  child.on('message', message => {
    if (message.type === 'ready') readyResolve();
    if (message.type === 'stats') { Object.assign(stats, message.stats); receivedStats = true; }
    if (message.type === 'error') { stats.errors.push(message.error); readyReject(new Error(message.error)); }
  });
  child.once('error', error => { children.delete(child); stats.errors.push(error.message); readyReject(error); });
  const kill = signal => { if (child.pid) try { process.kill(-child.pid, signal); } catch (error) { if (error.code !== 'ESRCH') throw error; } };
  const timer = setTimeout(() => { readyReject(new Error('Isolated mock readiness timed out')); kill('SIGKILL'); }, 10_000);
  try { await ready; } catch (error) { kill('SIGKILL'); throw error; } finally { clearTimeout(timer); }
  return { stats, close: async () => {
    requestedClose = true;
    if (!closed && child.connected) child.send({ type: 'close' });
    const timeout = setTimeout(() => { stats.errors.push('Isolated mock shutdown timed out'); kill('SIGKILL'); }, 5000);
    try { await exited; } finally { clearTimeout(timeout); }
  } };
}
if (process.argv[1] === new URL(import.meta.url).pathname) {
  assertRealExecutionEnv(process.env);
  let mock, closing = false;
  const close = async () => {
    if (closing) return; closing = true;
    await mock?.close();
    if (process.connected) await new Promise(done => process.send({ type: 'stats', stats: mock.stats }, done));
    if (process.connected) process.disconnect();
  };
  try {
    mock = await startMockLLM(resolve(process.env.FE_OUTPUT, 'llm-requests.jsonl'));
    process.on('message', message => { if (message.type === 'close') void close(); });
    process.on('disconnect', () => { void close(); });
    process.send({ type: 'ready' });
  } catch (error) {
    if (process.connected) process.send({ type: 'error', error: error.message });
    process.exitCode = 1; if (process.connected) process.disconnect();
  }
}
