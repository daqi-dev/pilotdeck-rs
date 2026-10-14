// Runner-side observation, independent of the Node net.connect preload.
// Production invokes ps/lsof only during real execution; unit tests inject output.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { appendFile } from 'node:fs/promises';
const exec = promisify(execFile);
// lsof exits 1 with empty stderr when some selected PID has no TCP fds or has
// exited since the ps snapshot; stdout still lists every matching live fd.
// Returns the usable stdout, or undefined when the failure must fail closed.
export function lsofExitOutput(error) {
  // A timed-out child can handle SIGTERM and exit 1: execFile then reports
  // killed=true, signal=null. That partial snapshot must still fail closed.
  if (error?.code === 1 && !error.stderr && !error.killed && !error.signal) return error.stdout ?? '';
  return undefined;
}
async function capture(file, args) {
  try {
    const result = await exec(file, args, { encoding: 'utf8', timeout: 5000, maxBuffer: 8 * 1024 * 1024 });
    if (result.stderr.trim()) throw new Error(result.stderr.trim());
    return result.stdout;
  }
  catch (error) {
    const output = file === 'lsof' ? lsofExitOutput(error) : undefined;
    if (output !== undefined) return output;
    throw new Error(`TCP audit ${file} failed: ${error.message}`);
  }
}
export function processTree(ps, rootPid, groupRoots = []) {
  const rows = ps.trim().split('\n').filter(Boolean).map(line => {
    const match = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/.exec(line);
    if (!match) throw new Error('Unrecognized ps process record');
    return { pid: Number(match[1]), ppid: Number(match[2]), pgid: Number(match[3]), command: match[4] };
  });
  const owned = new Set([rootPid]);
  // Include reparented descendants that remain in groups we actually spawned.
  for (const row of rows) if (groupRoots.includes(row.pgid)) owned.add(row.pid);
  let changed;
  do {
    changed = false;
    for (const row of rows) if (owned.has(row.ppid) && !owned.has(row.pid)) { owned.add(row.pid); changed = true; }
  } while (changed);
  if (!rows.some(row => row.pid === rootPid)) throw new Error('Runner absent from ps snapshot');
  return rows.filter(row => owned.has(row.pid));
}
export function tcpRecords(lsof) {
  const records = []; let pid, fd, type;
  for (const line of lsof.split('\n').filter(Boolean)) {
    const value = line.slice(1);
    if (line[0] === 'p') { pid = Number(value); fd = undefined; type = undefined; }
    else if (line[0] === 'f') { fd = value; type = undefined; }
    else if (line[0] === 't') type = value;
    else if (line[0] === 'n') {
      const target = value.includes('->') ? value.split('->').at(-1) : null;
      const destinationPort = target ? Number(/:(\d+)$/.exec(target)?.[1]) : null;
      const localPort = Number(/:(\d+)$/.exec(value.split('->')[0])?.[1]);
      if (!Number.isInteger(pid) || fd === undefined || !['IPv4', 'IPv6'].includes(type) || !Number.isInteger(localPort)
        || (target && !Number.isInteger(destinationPort))) throw new Error('Unrecognized lsof TCP endpoint');
      records.push({ pid, fd, type, endpoint: value, target, destinationPort, localPort });
    }
  }
  return records;
}
export function forbiddenTCP(records) {
  // Also fail a forbidden listener; do not confuse an ephemeral local port on
  // an outgoing connection with its destination port.
  return records.filter(row => [3001, 18789].includes(row.target ? row.destinationPort : row.localPort));
}
export async function startTCPAudit({ logFile, rootPid = process.pid, groupRoots = () => [], onFatal,
  run = capture, record = entry => appendFile(logFile, JSON.stringify(entry) + '\n'), intervalMs = 250 } = {}) {
  let timer, pending = Promise.resolve(), stopped = false, failure;
  const knownGroups = new Set();
  function assertSafe() { if (failure) throw failure; }
  function fail(error) {
    if (failure) return;
    failure = error;
    onFatal?.(error, [...knownGroups]);
  }
  async function sample(phase) {
    try {
      for (const pid of groupRoots()) if (Number.isInteger(pid)) knownGroups.add(pid);
      const processes = processTree(await run('ps', ['-axo', 'pid=,ppid=,pgid=,comm=']), rootPid, [...knownGroups]);
      for (const row of processes) if (row.pid !== rootPid && row.pid === row.pgid) knownGroups.add(row.pgid);
      const raw = await run('lsof', ['-nP', '-a', '-p', processes.map(row => row.pid).join(','), '-iTCP', '-F', 'pftnT']);
      const connections = tcpRecords(raw), forbidden = forbiddenTCP(connections);
      if (connections.some(row => !processes.some(proc => proc.pid === row.pid))) throw new Error('lsof returned a process outside the runner tree');
      // Signal first; evidence I/O must never postpone stopping a forbidden target.
      if (forbidden.length) fail(new Error(`FAIL: runner TCP target 3001/18789: ${forbidden.map(row => `${row.pid} ${row.endpoint}`).join(', ')}`));
      await record({ timestamp: new Date().toISOString(), phase, runnerPid: rootPid, processes, connections,
        rawLsof: raw, status: forbidden.length ? 'FAIL' : 'PASS', forbidden });
    } catch (error) {
      fail(error);
      await record({ timestamp: new Date().toISOString(), phase, status: 'FAIL', error: error.message }).catch(() => {});
    }
  }
  const enqueue = phase => { pending = pending.then(() => sample(phase)); return pending; };
  function schedule() {
    if (stopped || failure) return;
    timer = setTimeout(() => {
      void enqueue('during-run').then(schedule);
    }, intervalMs);
  }
  await sample('before-run'); assertSafe(); schedule();
  return { assertSafe, async checkpoint(phase) { await enqueue(phase); assertSafe(); },
    async stop() { stopped = true; clearTimeout(timer); await enqueue('after-cleanup'); assertSafe(); } };
}
