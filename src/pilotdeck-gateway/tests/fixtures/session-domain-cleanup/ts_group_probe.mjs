#!/usr/bin/env node
// Node v22 process-group probe for #164.  This checks the POSIX semantics the
// Python apparatus relies on; it is not TS product evidence.
//
// Safety: Node reaps the leader at its 'exit' event, so after that event a bare
// pid/pgid has no ownership proof any more.  Every spawned leader is tracked
// before the first await (exit listener + promise), and every actual signal is
// sent through signalHeldGroup()/signalHeldChild(), which refuse unless the
// leader is still a live, unreaped direct child.  A leader that exits during a
// bounded wait is reported as signalRefused, never signalled.  'clean' and
// 'leak' only use signal-0 observations after the leader's exit; 'leak' waits
// for a bounded short-lived grandchild to exit on its own.  Every wait is
// bounded.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const WAIT_EXIT_MS = 10000;
const WAIT_FILE_MS = 5000;
const WAIT_EMPTY_MS = 10000;
const LEAK_LINGER_SECONDS = 3;
const TERM_LINGER_SECONDS = 8;

function trackChild(child) {
  // Register before any await.  child.exitCode becomes non-null when Node has
  // reaped the leader and may reuse its pid/pgid; entry.exited is the single
  // ownership bit every signal path consults.
  const entry = {
    child, pid: child.pid, exited: false, exitCode: null, exitSignal: null,
  };
  entry.exitPromise = new Promise((resolve) => {
    child.once('exit', (code, signal) => {
      entry.exited = true;
      entry.exitCode = code;
      entry.exitSignal = signal;
      resolve({ code, signal });
    });
  });
  return entry;
}

function identityHeld(entry) {
  return !entry.exited
    && entry.child.exitCode === null
    && entry.child.signalCode === null;
}

function waitExit(entry, timeoutMs = WAIT_EXIT_MS) {
  return new Promise((resolve) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      resolve({ timedOut: true });
    }, timeoutMs);
    entry.exitPromise.then((exit) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(exit);
    });
  });
}

function signalHeldGroup(entry, signal) {
  if (!identityHeld(entry)) {
    return { sent: false, refused: true, reason: 'leader-exit-observed' };
  }
  try {
    process.kill(-entry.pid, signal);
    return { sent: true, refused: false, signal };
  } catch (error) {
    return { sent: false, refused: false, error: error.code || String(error) };
  }
}

function signalHeldChild(entry, signal) {
  if (!identityHeld(entry)) {
    return { sent: false, refused: true, reason: 'leader-exit-observed' };
  }
  try {
    entry.child.kill(signal);
    return { sent: true, refused: false, signal };
  } catch (error) {
    return { sent: false, refused: false, error: error.code || String(error) };
  }
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
  // Signal-0 observation only: never delivers a signal, so it is allowed after
  // the leader was reaped.  EPERM is BLOCKED, never empty.
  try {
    process.kill(-pgid, 0);
    return 'alive';
  } catch (error) {
    if (error.code === 'ESRCH') return 'empty';
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
  const seconds = (mode === 'term' || mode === 'term-early')
    ? TERM_LINGER_SECONDS : LEAK_LINGER_SECONDS;
  const spawnSleeper =
    'import subprocess,sys;'
    + `p=subprocess.Popen(['sleep','${seconds}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);`
    + "open(sys.argv[1],'w').write(str(p.pid))";
  if (mode === 'term-early' || mode === 'leak') return ['-I', '-c', spawnSleeper];
  return ['-I', '-c', spawnSleeper + ';import time;time.sleep(60)'];
}

async function run(mode, folder, inFlight) {
  const marker = join(folder, `${mode}.pid`);
  const args = leaderArgs(mode);
  if (mode !== 'clean') args.push(marker);
  const child = spawn('python3', args, { detached: true, stdio: 'ignore' });
  const entry = trackChild(child);
  // Register before any await: a throw below must still let the finally block
  // see this run's direct child and its ownership bit.
  inFlight.push(entry);
  const result = {
    mode, pgid: child.pid, grandchild: null, aliveAfterWait: null, signal: null,
    signalTarget: null, signalError: null, signalRefused: null,
    emptyAfterSignal: null, groupAfter: null,
  };
  if (mode === 'term') {
    if (!await waitFile(marker, WAIT_FILE_MS)) {
      throw new Error(`term grandchild marker missing after ${WAIT_FILE_MS}ms`);
    }
    result.grandchild = Number(readFileSync(marker, 'utf8'));
    result.aliveAfterWait = groupState(child.pid);
    // Single entry: refuses if the leader exited during the wait and Node has
    // already reaped it (its pid/pgid may be reusable).
    const attempt = signalHeldGroup(entry, 'SIGTERM');
    if (attempt.refused) {
      result.signalRefused = attempt.reason;
    } else if (attempt.sent) {
      result.signal = attempt.signal;
      result.signalTarget = 'group-while-leader-alive';
    } else {
      result.signalError = attempt.error;
    }
    const exit = await waitExit(entry);
    if (exit.timedOut) {
      throw new Error(`leader ${child.pid} did not exit within ${WAIT_EXIT_MS}ms`);
    }
    result.emptyAfterSignal = await settle(child.pid);
  } else if (mode === 'term-early') {
    if (!await waitFile(marker, WAIT_FILE_MS)) {
      throw new Error(`term-early grandchild marker missing after ${WAIT_FILE_MS}ms`);
    }
    result.grandchild = Number(readFileSync(marker, 'utf8'));
    // Deterministic race: wait for the leader's exit before asking the entry
    // to signal.  A call without the ownership gate would target a reaped pid.
    const exit = await waitExit(entry, WAIT_FILE_MS);
    if (exit.timedOut) {
      throw new Error(`term-early leader ${child.pid} did not exit within ${WAIT_FILE_MS}ms`);
    }
    const attempt = signalHeldGroup(entry, 'SIGTERM');
    if (attempt.refused) result.signalRefused = attempt.reason;
    else if (attempt.sent) {
      result.signal = attempt.signal;
      result.signalTarget = 'group-after-leader-exit';
    } else {
      result.signalError = attempt.error;
    }
    result.aliveAfterWait = groupState(child.pid);
    result.emptyAfterSignal = await settle(child.pid);
  } else {
    const exit = await waitExit(entry);
    if (exit.timedOut) {
      throw new Error(`leader ${child.pid} did not exit within ${WAIT_EXIT_MS}ms`);
    }
    // The 'exit' event reaped the leader.  Only observation is allowed now.
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
  results.push(await run('term-early', folder, inFlight));
  // Independent gate self-test: an entry that already observed exit must be
  // refused without ever calling process.kill.  The pid is far outside any
  // live group; if a future regression removes the gate, the call errors
  // instead of reaching a real target.
  const fakeEntry = {
    pid: 2147483647, exited: true, exitCode: 0, exitSignal: null,
    child: { exitCode: 0, signalCode: null },
  };
  const fake = signalHeldGroup(fakeEntry, 'SIGTERM');
  results.push({ mode: 'fake-exited-refusal', ...fake });
  if (!(fake.refused === true && fake.sent === false)) status = 'FAIL';
} catch (error) {
  status = 'FAIL';
  results.push({ error: String(error) });
} finally {
  for (const entry of inFlight) {
    // Only a direct child whose 'exit' event has not fired is still an
    // unreaped, Node-owned process.  Never touch a grandchild or a bare pgid.
    signalHeldChild(entry, 'SIGKILL');
  }
  rmSync(folder, { recursive: true, force: true });
}
if (status === 'PASS') {
  const clean = results[0];
  const leak = results[1];
  const term = results[2];
  const early = results[3];
  const fake = results[4];
  status = clean && leak && term && early && fake
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
    && term.signalRefused === null
    && term.emptyAfterSignal === true
    && term.groupAfter === 'empty'
    && early.signal === null
    && early.signalTarget === null
    && early.signalError === null
    && early.signalRefused === 'leader-exit-observed'
    && early.emptyAfterSignal === true
    && early.groupAfter === 'empty'
    && fake.refused === true && fake.sent === false
    ? 'PASS' : 'BLOCKED';
}
console.log(JSON.stringify({ status, results }, null, 2));
process.exitCode = status === 'PASS' ? 0 : 1;
