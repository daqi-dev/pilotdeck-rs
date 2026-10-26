#!/usr/bin/env node
// Node v22 process-group probe for #164.  This checks the POSIX semantics the
// Python apparatus relies on; it is not TS product evidence.
//
// Safety change (fix-r1): Node's 'exit' event reaps the leader, so after it the
// bare pgid is no longer owned by this run.  This probe therefore never signals
// -pgid after exit; leak cleanup targets only the known live grandchild pid
// (whose pid cannot be reused while the process exists).  All waits are bounded.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const WAIT_EXIT_MS = 10000;

function waitExit(child) {
  return new Promise((resolve) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { child.kill('SIGKILL'); } catch (error) { /* already gone */ }
      resolve({ timedOut: true });
    }, WAIT_EXIT_MS);
    child.once('exit', (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ code, signal });
    });
  });
}

function pidAlive(pid) {
  if (!pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return false;
  }
}

function groupState(pgid) {
  try {
    process.kill(-pgid, 0);
    return 'alive';
  } catch (error) {
    if (error.code === 'ESRCH') return 'empty';
    // macOS can report EPERM for a signal raced by the last live member
    // exiting, and for a group whose only member is the unreaped zombie
    // leader.  Node reaps the leader at 'exit', so the caller retries instead
    // of calling it empty.
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
  // Register before any await: a throw in waitExit/readFileSync must still let
  // the finally block see this run's direct child pid.
  const entry = { pgid, childPid: child.pid, grandchild: null, exited: false };
  inFlight.push(entry);
  const exit = await waitExit(child);
  if (exit.timedOut) throw new Error(`leader ${child.pid} did not exit within ${WAIT_EXIT_MS}ms`);
  entry.exited = true;
  const grandchild = mode === 'leak' ? Number(readFileSync(marker, 'utf8')) : null;
  entry.grandchild = grandchild;
  const aliveAfterWait = groupState(pgid);
  let signal = null;
  let signalTarget = null;
  let signalError = null;
  let emptyAfterSignal = null;
  if (mode === 'leak') {
    // The leader is reaped here; -pgid is not provably ours anymore.  Signal
    // the known live grandchild pid instead and wait for the group to empty.
    signal = 'SIGTERM';
    signalTarget = 'grandchild';
    try {
      process.kill(grandchild, 'SIGTERM');
    } catch (error) {
      signalError = error.code;
    }
    emptyAfterSignal = await settle(pgid);
  }
  return { mode, pgid, grandchild, aliveAfterWait, signal, signalTarget, signalError,
           emptyAfterSignal, groupAfter: groupState(pgid) };
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
  for (const entry of inFlight) {
    // Only pids whose liveness is still ours to prove: an unexited direct
    // child, and a grandchild while its group is still alive (then that pid is
    // the live group member, not a reused pid).  Never a bare -pgid after the
    // leader's 'exit' reaped it.
    if (!entry.exited && pidAlive(entry.childPid)) {
      try {
        process.kill(entry.childPid, 'SIGKILL');
      } catch (error) { /* already gone */ }
    }
    if (entry.grandchild) {
      let state = null;
      try { state = groupState(entry.pgid); } catch (error) { state = 'unknown'; }
      if (state === 'alive') {
        try {
          process.kill(entry.grandchild, 'SIGKILL');
        } catch (error) { /* already gone */ }
      }
    }
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
    && leak.signalTarget === 'grandchild'
    && leak.emptyAfterSignal === true
    && leak.groupAfter === 'empty'
    ? 'PASS' : 'BLOCKED';
}
console.log(JSON.stringify({ status, node: process.version, results }, null, 2));
process.exit(status === 'PASS' ? 0 : 1);
