#!/usr/bin/env node
// Node v22 process-group probe for #164.  This checks the POSIX semantics the
// Python apparatus relies on; it is not TS product evidence.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function waitExit(child) {
  return new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal })));
}

function groupState(pgid) {
  try {
    process.kill(-pgid, 0);
    return 'alive';
  } catch (error) {
    if (error.code === 'ESRCH') return 'empty';
    // macOS sandbox can report EPERM for a signal raced by the last member
    // exiting; the caller retries instead of calling it empty.
    if (error.code === 'EPERM') return 'blocked';
    throw error;
  }
}

async function settle(pgid, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (groupState(pgid) === 'empty') return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return groupState(pgid) === 'empty';
}

async function run(mode, folder) {
  const marker = join(folder, `${mode}.pid`);
  const code = mode === 'leak'
    ? "import subprocess,sys; p=subprocess.Popen(['sleep','60'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); open(sys.argv[1],'w').write(str(p.pid))"
    : 'pass';
  const args = mode === 'leak' ? ['-I', '-c', code, marker] : ['-I', '-c', code];
  const child = spawn('python3', args, { detached: true, stdio: 'ignore' });
  const pgid = child.pid;
  await waitExit(child);
  const grandchild = mode === 'leak' ? Number(readFileSync(marker, 'utf8')) : null;
  const aliveAfterWait = groupState(pgid);
  let signal = null;
  let signalError = null;
  let emptyAfterSignal = null;
  if (aliveAfterWait !== 'empty') {
    for (const sig of ['SIGTERM', 'SIGKILL']) {
      signal = sig;
      try {
        process.kill(-pgid, sig);
      } catch (error) {
        if (error.code === 'ESRCH') {
          emptyAfterSignal = true;
          break;
        }
        signalError = error.code;
      }
      if (await settle(pgid)) {
        emptyAfterSignal = true;
        break;
      }
    }
  }
  inFlight.push({ pgid, grandchild });
  return { mode, pgid, grandchild, aliveAfterWait, signal, signalError, emptyAfterSignal,
           groupAfter: groupState(pgid) };
}

const folder = mkdtempSync(join(tmpdir(), 'pdrs164-node-probe-'));
const inFlight = [];
const results = [];
let status = 'PASS';
try {
  results.push(await run('clean', folder));
  results.push(await run('leak', folder));
} catch (error) {
  status = 'FAIL';
  results.push({ error: String(error) });
} finally {
  for (const { pgid, grandchild } of inFlight) {
    try {
      if (grandchild) process.kill(grandchild, 'SIGKILL');
    } catch (error) { /* already gone */ }
    try {
      process.kill(-pgid, 'SIGKILL');
    } catch (error) { /* already gone */ }
  }
  rmSync(folder, { recursive: true, force: true });
}
if (status === 'PASS') {
  const clean = results[0];
  const leak = results[1];
  status = clean && leak
    && clean.aliveAfterWait === 'empty'
    && leak.aliveAfterWait === 'alive'
    && leak.signal === 'SIGTERM'
    && leak.emptyAfterSignal === true
    && leak.groupAfter === 'empty'
    ? 'PASS' : 'BLOCKED';
}
console.log(JSON.stringify({ status, node: process.version, results }, null, 2));
process.exit(status === 'PASS' ? 0 : 1);
