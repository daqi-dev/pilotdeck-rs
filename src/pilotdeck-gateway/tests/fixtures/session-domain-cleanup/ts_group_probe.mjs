#!/usr/bin/env node
// Node v22 process-group probe for #164.  This checks the POSIX semantics the
// Python apparatus relies on; it is not TS product evidence.
//
// Safety: Node reaps the leader at its 'exit' event, so after that event a bare
// pid/pgid has no ownership proof any more.  This probe therefore sends a group
// signal only in 'term' mode, while the leader is still a live, unreaped direct
// child.  'leak' only observes a bounded short-lived grandchild and waits for it
// to exit on its own; no signal is ever sent to a pid Node does not hold.  Every
// wait is bounded.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const WAIT_EXIT_MS = 10000;
const WAIT_FILE_MS = 5000;
const WAIT_EMPTY_MS = 10000;
const LEAK_LINGER_SECONDS = 3;

function waitExit(child) {
  return new Promise((resolve) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
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

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFile(path, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(path)) return true;
    await sleep(20);
  }
  return existsSync(path);
}

function groupState(pgid) {
  try {
    process.kill(-pgid, 0);
    return 'alive';
  } catch (error) {
    if (error.code === 'ESRCH') return 'empty';
    // macOS also reports EPERM for an unowned/reused group or a group whose
    // only member is a zombie.  Never read it as empty: the caller must not
    // signal on it and only reports BLOCKED.
    if (error.code === 'EPERM') return 'blocked';
    throw error;
  }
}

async function settle(pgid, timeoutMs = WAIT_EMPTY_MS) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (groupState(pgid) === 'empty') return true;
    await sleep(50);
  }
  return groupState(pgid) === 'empty';
}

function leaderArgs(mode) {
  if (mode === 'clean') return ['-I', '-c', 'pass'];
  const seconds = mode === 'term' ? 8 : LEAK_LINGER_SECONDS;
  const spawnSleeper =
    'import subprocess,sys;'
    + `p=subprocess.Popen(['sleep','${seconds}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);`
    + "open(sys.argv[1],'w').write(str(p.pid))";
  if (mode === 'leak') return ['-I', '-c', spawnSleeper];
  return ['-I', '-c', spawnSleeper + ';import time;time.sleep(60)'];
}

async function run(mode, folder, inFlight) {
  const marker = join(folder, `${mode}.pid`);
  const args = leaderArgs(mode);
  if (mode !== 'clean') args.push(marker);
  const child = spawn('python3', args, { detached: true, stdio: 'ignore' });
  const entry = { child, pgid: child.pid, exited: false };
  // Register before any await: a throw below must still let the finally block
  // see this run's direct child.
  inFlight.push(entry);
  const result = {
    mode, pgid: child.pid, grandchild: null, aliveAfterWait: null, signal: null,
    signalTarget: null, signalError: null, emptyAfterSignal: null, groupAfter: null,
  };
  if (mode === 'term') {
    if (!await waitFile(marker, WAIT_FILE_MS)) {
      throw new Error(`term grandchild marker missing after ${WAIT_FILE_MS}ms`);
    }
    result.grandchild = Number(readFileSync(marker, 'utf8'));
    result.aliveAfterWait = groupState(child.pid);
    // Safe signal window: child.pid is a live, not-yet-reaped direct child, so
    // -pgid still names this run's group and the leader itself.
    try {
      process.kill(-child.pid, 'SIGTERM');
      result.signal = 'SIGTERM';
      result.signalTarget = 'group-while-leader-alive';
    } catch (error) {
      result.signalError = error.code || String(error);
    }
    const exit = await waitExit(child);
    if (exit.timedOut) {
      throw new Error(`leader ${child.pid} did not exit within ${WAIT_EXIT_MS}ms`);
    }
    entry.exited = true;
    result.emptyAfterSignal = await settle(child.pid);
  } else {
    const exit = await waitExit(child);
    if (exit.timedOut) {
      throw new Error(`leader ${child.pid} did not exit within ${WAIT_EXIT_MS}ms`);
    }
    // The 'exit' event reaped the leader.  Only observation is allowed now.
    entry.exited = true;
    result.aliveAfterWait = groupState(child.pid);
    if (mode === 'leak') {
      if (!existsSync(marker)) throw new Error('leak grandchild marker missing');
      result.grandchild = Number(readFileSync(marker, 'utf8'));
      // No signal: the bare pgid/pid is not provably ours.  The grandchild is
      // short-lived, so a bounded wait observes its own exit.
      result.emptyAfterSignal = await settle(child.pid);
    }
  }
  result.groupAfter = groupState(child.pid);
  return result;
}

const folder = mkdtempSync(join(tmpdir(), 'pdrs164-node-probe-'));
const inFlight = [];
const results = [];
let status = 'PASS';
try {
  results.push(await run('clean', folder, inFlight));
  results.push(await run('leak', folder, inFlight));
  results.push(await run('term', folder, inFlight));
} catch (error) {
  status = 'FAIL';
  results.push({ error: String(error) });
} finally {
  for (const entry of inFlight) {
    // Only a direct child whose 'exit' event has not fired is still an
    // unreaped, Node-owned process.  Never touch a grandchild or a bare pgid.
    if (!entry.exited) {
      try {
        entry.child.kill('SIGKILL');
      } catch (error) { /* already gone */ }
    }
  }
  rmSync(folder, { recursive: true, force: true });
}
if (status === 'PASS') {
  const clean = results[0];
  const leak = results[1];
  const term = results[2];
  status = clean && leak && term
    && clean.aliveAfterWait === 'empty'
    && clean.groupAfter === 'empty'
    && leak.aliveAfterWait === 'alive'
    && leak.signal === null
    && leak.signalTarget === null
    && leak.emptyAfterSignal === true
    && leak.groupAfter === 'empty'
    && term.aliveAfterWait === 'alive'
    && term.signal === 'SIGTERM'
    && term.signalTarget === 'group-while-leader-alive'
    && term.signalError === null
    && term.emptyAfterSignal === true
    && term.groupAfter === 'empty'
    ? 'PASS' : 'BLOCKED';
}
console.log(JSON.stringify({ status, node: process.version, results }, null, 2));
process.exit(status === 'PASS' ? 0 : 1);
