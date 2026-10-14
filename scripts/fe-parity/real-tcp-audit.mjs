// Runner-side observation, independent of the Node net.connect preload.
// Production invokes ps/lsof only during real execution; unit tests inject output.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { appendFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { resolve, dirname, basename } from 'node:path';
import { userInfo } from 'node:os';
import { baseline } from './env.mjs';
import { requiredChromeFlags } from './real-browser.mjs';
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
const chromeExecutable = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const chromeHelper = /^\/Applications\/Google Chrome\.app\/Contents\/Frameworks\/Google Chrome Framework\.framework\/Versions\/[^/]+\/(?:Helpers\/Google Chrome Helper(?: \([^)]+\))?\.app\/Contents\/MacOS\/Google Chrome Helper(?: \([^)]+\))?|chrome_crashpad_handler)(?:\s|$)/;
const soffice = /(?:^|[\s/])soffice(?:\.bin)?(?:\s|$)/;
const updater = /(?:^|[\s/])GoogleUpdater(?:\s|$|\/)/;
const serverCommand = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
const licenseArgv = ['/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild', '-license', 'check'];
const developerTool = /(?:^|\s|[;&|])(?:[^\s;|]*\/)?(?:xcodebuild|xcrun)(?=\s|$|[;&|])/;
const gitOrXcrun = /^(?:git|xcrun|\/[^\s]+\/(?:git|xcrun))(?:\s|$)/;
const optionalEnvironment = row => updater.test(row.command) || soffice.test(row.command);
const nodeProcess = row => row.command.startsWith(process.execPath + ' ');
export function environmentTargets(processes, { nodeRoots = [] } = {}) {
  const hasLicenseCheck = processes.some(row => row.command === licenseArgv.join(' '));
  return processes.filter(row => optionalEnvironment(row) || nodeProcess(row) || nodeRoots.some(root => root.pid === row.pid)
    || hasLicenseCheck && gitOrXcrun.test(row.command) || row.command === licenseArgv.join(' '));
}
// macOS ps -E appends the environment to args (support a prepended environment
// too). Remove the already captured argv before extracting HOME; do not persist
// raw environment text, which can contain credentials. Bind the two snapshots
// by PID/PPID/PGID and exact argv, and fail closed on missing/ambiguous HOME.
export function processHomes(raw, processes, { unreadable } = {}) {
  let rows;
  try { rows = processTree(raw, processes[0].pid, processes.map(row => row.pgid)); }
  catch (error) { throw new Error(`FAIL: environment snapshot invalid: ${error.message}`); }
  const homes = new Map();
  for (const expected of processes) {
    const matching = rows.filter(item => item.pid === expected.pid);
    const row = matching.length === 1 ? matching[0] : undefined;
    if (!row || row.ppid !== expected.ppid || row.pgid !== expected.pgid) throw new Error(`FAIL: environment process identity changed for PID ${expected.pid}`);
    // Only an identical argv with no environment is a hardened-process fallback.
    // Missing rows, changed argv and exposed but malformed HOME never qualify.
    if (row.command === expected.command && unreadable && optionalEnvironment(expected)) {
      unreadable.add(row.pid); continue;
    }
    let environment;
    if (row.command.startsWith(expected.command + ' ')) environment = row.command.slice(expected.command.length + 1);
    else if (row.command.endsWith(' ' + expected.command)) environment = row.command.slice(0, -expected.command.length - 1);
    else throw new Error(`FAIL: ps -E did not expose environment with unchanged argv for PID ${expected.pid}`);
    const matches = [...environment.matchAll(/(?:^|\s)HOME=(.*?)(?=\s[A-Za-z_][A-Za-z0-9_]*=|$)/g)];
    if (matches.length !== 1 || !matches[0][1]) throw new Error(`FAIL: ps -E HOME missing or ambiguous for PID ${expected.pid}`);
    homes.set(row.pid, matches[0][1]);
  }
  return homes;
}
// Chrome identity is rebuilt from the current snapshot; helper titles may change.
// No cached browser approval or Chrome HOME is evidence for a current chain.
export function forbiddenProcesses(processes, { playwrightRoots = [], browserTmpRoots = [], browserEvidence = [],
  runtimeRoots = [], processHOME = new Map(), operatorHome = userInfo().homedir, deferEnvironment = false,
  unreadable = new Set(), alternativeEvidence = [], readOnlySystemChecks = [], updaterRealpath = realpathSync } = {}) {
  const byPid = new Map(processes.map(row => [row.pid, row]));
  function ancestor(row, ids) {
    const seen = new Set();
    for (let parent = row.ppid; byPid.has(parent) && !seen.has(parent); parent = byPid.get(parent).ppid) {
      if (ids.has(parent)) return parent;
      seen.add(parent);
    }
  }
  const homeMatches = (row, home) => deferEnvironment || processHOME.get(row.pid) === home
    || unreadable.has(row.pid) && optionalEnvironment(row);
  const evidence = (row, proof) => {
    if (!deferEnvironment && unreadable.has(row.pid)) alternativeEvidence.push({ pid: row.pid, environment: 'unreadable', ...proof });
  };
  const playwright = new Set(playwrightRoots);
  const currentMainChrome = new Set();
  const profilesByPid = new Map();
  for (const row of processes) {
    const profiles = [...row.command.matchAll(/(?:^|\s)--user-data-dir=(.+?)(?=\s--|$)/g)];
    const profile = profiles.length === 1 ? profiles[0][1] : undefined;
    const flags = requiredChromeFlags.every(flag => row.command.split(/\s+/).filter(arg => arg === flag).length === 1);
    if (row.command.startsWith(chromeExecutable + ' ') && ancestor(row, playwright) && flags
      && /(?:^|\s)--remote-debugging-pipe(?:\s|$)/.test(row.command)
      && profile && browserTmpRoots.some(directory => directory && dirname(resolve(profile)) === resolve(directory)
        && basename(profile).startsWith('playwright_chromiumdev_profile-'))) {
      currentMainChrome.add(row.pid);
      profilesByPid.set(row.pid, profile);
      if (!deferEnvironment) browserEvidence.push({ pid: row.pid, environment: 'not-checked',
        source: 'current Playwright ancestor, temporary user-data-dir and required Chrome flags',
        playwrightAncestorPid: ancestor(row, playwright), chromeAncestorPid: row.pid, userDataDir: profile });
    }
  }
  // Traverse the complete current chain to the verified main browser, even
  // when a helper precedes its parent in ps output. Never reuse an orphan.
  const checkedHelpers = new Set();
  for (const row of processes) if (chromeHelper.test(row.command)) {
    const chromeAncestorPid = ancestor(row, currentMainChrome);
    if (!chromeAncestorPid) continue;
    const profiles = [...row.command.matchAll(/(?:^|\s)--user-data-dir=(.+?)(?=\s--|$)/g)];
    if (profiles.length && (profiles.length !== 1 || resolve(profiles[0][1]) !== resolve(profilesByPid.get(chromeAncestorPid)))) continue;
    checkedHelpers.add(row.pid);
    if (!deferEnvironment) browserEvidence.push({ pid: row.pid, environment: 'not-checked',
      source: 'current bundle helper/crashpad ancestor chain to verified Playwright Chrome',
      chromeAncestorPid, userDataDir: profilesByPid.get(chromeAncestorPid) });
  }
  const runtimes = new Map(runtimeRoots.map(row => [row.pid, row.home]));
  const servers = new Map();
  const gitAncestors = new Map();
  for (const row of processes) {
    const runtime = ancestor(row, runtimes), home = runtimes.get(runtime);
    if (home && home !== operatorHome && row.command === serverCommand && homeMatches(row, home)) servers.set(row.pid, home);
    if (home && home !== operatorHome && gitOrXcrun.test(row.command) && homeMatches(row, home)) gitAncestors.set(row.pid, home);
  }
  const updaterRoot = resolve(operatorHome, 'Library/Application Support/Google/GoogleUpdater') + '/';
  const wakeCandidates = new Map(), allowedUpdaters = new Map();
  for (const row of processes) {
    // ps has flattened argv: require the exact two-element spelling, including
    // the single space before the only argument. No other updater mode qualifies.
    const match = /^(.*\/GoogleUpdater) (--wake|--wake-all)$/.exec(row.command);
    if (!match) continue;
    const executable = match[1], relative = executable.startsWith(updaterRoot) ? executable.slice(updaterRoot.length) : '';
    if (!/^[^/\s]+\/GoogleUpdater\.app\/Contents\/MacOS\/GoogleUpdater$/.test(relative)
      || resolve(executable) !== executable || !homeMatches(row, operatorHome)) continue;
    try { if (updaterRealpath(executable) !== executable) continue; }
    catch { continue; } // Missing/unobservable/noncanonical installed executable fails closed.
    wakeCandidates.set(row.pid, { executable, mode: match[2] });
  }
  // A fresh Chrome and its updater can first appear together, in any ps order.
  // Only a currently verified main browser or an authorized same-path wake-all
  // direct parent qualifies; helpers, cached browsers and group membership do not.
  let changed;
  do {
    changed = false;
    for (const row of processes) {
      const candidate = wakeCandidates.get(row.pid), parent = allowedUpdaters.get(row.ppid);
      if (!candidate || allowedUpdaters.has(row.pid)) continue;
      // Chrome detaches its wake-all updater, which launchd then adopts (ppid 1);
      // it only reaches this tree through a process group we spawned.
      const adoptedWakeAll = row.ppid === 1 && candidate.mode === '--wake-all';
      if (!currentMainChrome.has(row.ppid) && !adoptedWakeAll
        && !(parent?.mode === '--wake-all' && parent.executable === candidate.executable)) continue;
      const proof = { ...candidate, chromeAncestorPid: currentMainChrome.has(row.ppid) ? row.ppid
        : adoptedWakeAll ? null : parent.chromeAncestorPid };
      allowedUpdaters.set(row.pid, proof); changed = true;
      evidence(row, { alternative: 'canonical installed user updater; exact wake argv and current verified direct parent chain',
        executable: proof.executable, realpath: proof.executable, argv: [proof.executable, proof.mode],
        parentPid: row.ppid, chromeAncestorPid: proof.chromeAncestorPid });
    }
  } while (changed);
  return processes.filter(row => {
    if (/(?:^|\s|[;&|])(?:[^\s;|]*\/)?(?:open|xdg-open|osascript)(?=\s|$|[;&|])/.test(row.command)) return true;
    if (developerTool.test(row.command)) {
      // ps supplies flattened args: demand the exact three-element spelling,
      // no shell wrapper, extra switches, alternate bundle, or cached ancestry.
      const source = ancestor(row, servers) ?? ancestor(row, gitAncestors);
      const home = servers.get(source) ?? gitAncestors.get(source);
      if (row.command !== licenseArgv.join(' ') || !source || !homeMatches(row, home)) return true;
      if (!deferEnvironment) readOnlySystemChecks.push({ ...row, argv: [...licenseArgv], ancestorPid: source,
        HOME: processHOME.get(row.pid), reason: 'read-only Xcode license check; exact argv and current owned service ancestry' });
      return false;
    }
    if (updater.test(row.command)) {
      return !allowedUpdaters.has(row.pid);
    }
    if (soffice.test(row.command)) {
      const server = ancestor(row, servers);
      const allowed = server && homeMatches(row, servers.get(server));
      if (allowed) evidence(row, { alternative: 'current tested server ancestor with verified isolated HOME; officePreview execFile inherits env',
        serverAncestorPid: server, inheritedHOME: servers.get(server), source: 'ui/server/services/officePreview.js:235-240,411-415' });
      return !allowed;
    }
    if (!row.command.includes('/Applications/')) return false;
    if (currentMainChrome.has(row.pid) || checkedHelpers.has(row.pid)) return false;
    return true;
  });
}
export async function startTCPAudit({ logFile, rootPid = process.pid, groupRoots = () => [], onFatal,
  playwrightRoots = () => [], browserTmpRoots = () => [], runtimeRoots = () => [],
  nodeRoots = () => [],
  updaterRealpath = realpathSync,
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
      const processes = processTree(await run('ps', ['-ww', '-axo', 'pid=,ppid=,pgid=,args=']), rootPid, [...knownGroups]);
      for (const row of processes) if (row.pid !== rootPid && row.pid === row.pgid) knownGroups.add(row.pgid);
      const options = { playwrightRoots: playwrightRoots(), browserTmpRoots: browserTmpRoots(), runtimeRoots: runtimeRoots(), nodeRoots: nodeRoots(), updaterRealpath };
      // Structural violations terminate before a second ps call or lsof. The
      // preliminary pass and final pass both require current browser ancestry.
      let forbiddenApps = forbiddenProcesses(processes, { ...options, deferEnvironment: true });
      let homes = new Map();
      const unreadable = new Set(), alternativeEvidence = [];
      const readOnlySystemChecks = [];
      const browserEvidence = [];
      if (!forbiddenApps.length) {
        const targets = environmentTargets(processes, options).filter(row => row.pid !== rootPid);
        if (targets.length) homes = processHomes(await run('ps', ['-ww', '-E', '-p', targets.map(row => row.pid).join(','), '-o', 'pid=,ppid=,pgid=,args=']), targets, { unreadable });
        // Directly spawned Node roots and all observed Node descendants must
        // expose HOME and match their own mode/mock root, never the operator.
        const roots = new Map([...options.runtimeRoots, ...options.nodeRoots].map(row => [row.pid, row.home]));
        const byPid = new Map(processes.map(row => [row.pid, row]));
        for (const row of targets.filter(row => nodeProcess(row) || roots.has(row.pid))) {
          const seen = new Set(); let parent = row.pid;
          while (!roots.has(parent) && byPid.has(parent) && !seen.has(parent)) { seen.add(parent); parent = byPid.get(parent).ppid; }
          const home = roots.get(parent);
          if (!home || home === userInfo().homedir || homes.get(row.pid) !== home) throw new Error(`FAIL: Node process ${row.pid} HOME missing or outside isolated service root`);
        }
        forbiddenApps = forbiddenProcesses(processes, { ...options, processHOME: homes, unreadable, alternativeEvidence, readOnlySystemChecks, browserEvidence });
      }
      const environmentEvidence = [...homes].map(([pid, HOME]) => ({ pid, HOME, environment: 'readable', source: 'ps -ww -E; identity and argv matched' }));
      for (const pid of unreadable) environmentEvidence.push(alternativeEvidence.find(row => row.pid === pid)
        ?? { pid, environment: 'unreadable', alternative: 'none; structural authorization failed' });
      // Fail before lsof and evidence I/O, even if either subsequently hangs.
      if (forbiddenApps.length) {
        fail(new Error(`FAIL: runner forbidden process: ${forbiddenApps.map(row => `${row.pid} ${row.command}`).join(', ')}`));
        await record({ timestamp: new Date().toISOString(), phase, runnerPid: rootPid, processes, environmentEvidence, browserEvidence, readOnlySystemChecks, status: 'FAIL', forbiddenProcesses: forbiddenApps });
        return;
      }
      const raw = await run('lsof', ['-nP', '-a', '-p', processes.map(row => row.pid).join(','), '-iTCP', '-F', 'pftnT']);
      const connections = tcpRecords(raw), forbidden = forbiddenTCP(connections);
      if (connections.some(row => !processes.some(proc => proc.pid === row.pid))) throw new Error('lsof returned a process outside the runner tree');
      // Signal first; evidence I/O must never postpone stopping a forbidden target.
      if (forbidden.length) fail(new Error(`FAIL: runner TCP target 3001/18789: ${forbidden.map(row => `${row.pid} ${row.endpoint}`).join(', ')}`));
      await record({ timestamp: new Date().toISOString(), phase, runnerPid: rootPid, processes, environmentEvidence, readOnlySystemChecks, connections,
        browserEvidence, rawLsof: raw, status: forbidden.length ? 'FAIL' : 'PASS', forbidden, forbiddenProcesses: forbiddenApps });
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
