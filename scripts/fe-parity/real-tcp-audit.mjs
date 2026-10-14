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
async function capture(file, args, { timeout = 5000, killSignal = 'SIGTERM' } = {}) {
  try {
    const result = await exec(file, args, { encoding: 'utf8', timeout, killSignal, maxBuffer: 8 * 1024 * 1024 });
    if (result.stderr.trim()) throw new Error(result.stderr.trim());
    return result.stdout;
  }
  catch (error) {
    // Targeted ps exits 1 with no output/stderr when the selected PID is gone.
    // A timeout, signal, diagnostic or partial output never proves an exit.
    const output = file === 'lsof' ? lsofExitOutput(error)
      : file === 'ps' && args.includes('-p') && !error.stdout ? lsofExitOutput(error) : undefined;
    if (output !== undefined) return output;
    throw new Error(`TCP audit ${file} failed: ${error.message}`);
  }
}
function processRows(ps) {
  return ps.trim().split('\n').filter(Boolean).map(line => {
    const match = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+)$/.exec(line);
    if (!match) throw new Error('Unrecognized ps process record');
    return { pid: Number(match[1]), ppid: Number(match[2]), pgid: Number(match[3]), command: match[4] };
  });
}
export function processTree(ps, rootPid, groupRoots = []) {
  const rows = processRows(ps);
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
const unreadableCommand = command => /^\(.+\)$/.test(command);
const defunctCommand = command => /(?:^|\s)<defunct>(?:\s|$)/.test(command);
// args=(comm) is not evidence of an executable. Re-probe each sample, binding
// comm/stat to the current PID/PPID/PGID; never reuse a previous path approval.
export async function resolveUnreadableProcesses(processes, { run = capture, executableRealpath = realpathSync } = {}) {
  const resolved = [];
  for (const row of processes) {
    if (!unreadableCommand(row.command) && !defunctCommand(row.command)) { resolved.push(row); continue; }
    const observation = { source: 'ps -ww -p; pid/ppid/pgid/stat/comm', status: 'unavailable' };
    const probe = async () => {
      const raw = (await run('ps', ['-ww', '-p', String(row.pid), '-o', 'pid=,ppid=,pgid=,stat=,comm='])).trim();
      if (!raw) { observation.status = 'exited'; delete observation.stat; return; }
      const match = /^(\d+)\s+(\d+)\s+(\d+)\s+(\S+)(?:\s+(.+))?$/.exec(raw);
      if (!match || Number(match[1]) !== row.pid || Number(match[2]) !== row.ppid || Number(match[3]) !== row.pgid)
        throw new Error('executable probe process identity changed or malformed');
      observation.stat = match[4];
      if (match[4].startsWith('Z') || defunctCommand(match[5] ?? '')) { observation.status = 'zombie'; return; }
      const path = match[5];
      if (path?.startsWith('/') && resolve(path) === path) observation.path = path;
    };
    try {
      if (defunctCommand(row.command)) { observation.status = 'zombie'; observation.source = '<defunct> in current ps args snapshot'; }
      else {
        await probe();
        if (observation.path) {
          try {
            observation.realpath = executableRealpath(observation.path);
            if (!observation.realpath.startsWith('/') || resolve(observation.realpath) !== observation.realpath)
              throw new Error('noncanonical executable realpath');
            observation.status = 'resolved';
          } catch (error) {
            // The executable can disappear after comm. Only a fresh exit/zombie
            // observation allows that race; a still-live unknown path fails.
            observation.error = error.message;
            await probe();
          }
        }
      }
    } catch (error) { observation.status = 'unavailable'; observation.error = error.message; }
    resolved.push({ ...row, executableObservation: observation });
  }
  return resolved;
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
const sofficeExecutable = '/Applications/LibreOffice.app/Contents/MacOS/soffice';
const updater = /(?:^|[\s/])GoogleUpdater(?:\s|$|\/)/;
const serverCommand = `${process.execPath} --import tsx ${resolve(baseline, 'server/index.js')}`;
const licenseArgv = ['/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild', '-license', 'check'];
const developerTool = /(?:^|\s|[;&|])(?:[^\s;|]*\/)?(?:xcodebuild|xcrun)(?=\s|$|[;&|])/;
const gitOrXcrun = /^(?:git|xcrun|\/[^\s]+\/(?:git|xcrun))(?:\s|$)/;
const executablePaths = row => [row.executableObservation?.path, row.executableObservation?.realpath].filter(Boolean);
// ps stat E = exiting, Z = zombie: the executable can no longer be resolved.
const inactiveProcess = row => defunctCommand(row.command) || ['exited', 'zombie'].includes(row.executableObservation?.status)
  || /[EZ]/.test(row.executableObservation?.stat ?? '');
const optionalEnvironment = row => [row.command, ...executablePaths(row)].some(value => updater.test(value) || soffice.test(value));
const nodeProcess = row => row.command.startsWith(process.execPath + ' ') || executablePaths(row).includes(process.execPath);
export function environmentTargets(processes, { nodeRoots = [] } = {}) {
  const hasLicenseCheck = processes.some(row => row.command === licenseArgv.join(' '));
  return processes.filter(row => !inactiveProcess(row) && (optionalEnvironment(row) || nodeProcess(row) || nodeRoots.some(root => root.pid === row.pid)
    || hasLicenseCheck && gitOrXcrun.test(row.command) || row.command === licenseArgv.join(' ')));
}
// macOS ps -E appends the environment to args (support a prepended environment
// too). Remove the already captured argv before extracting HOME; do not persist
// raw environment text, which can contain credentials. Bind the two snapshots
// by PID/PPID/PGID and exact argv, and fail closed on missing/ambiguous HOME.
export function processHomes(raw, processes, { unreadable } = {}) {
  let rows;
  try { rows = processRows(raw); }
  catch (error) { throw new Error(`FAIL: environment snapshot invalid: ${error.message}`); }
  const homes = new Map();
  for (const expected of processes) {
    const matching = rows.filter(item => item.pid === expected.pid);
    const row = matching.length === 1 ? matching[0] : undefined;
    if (!matching.length) throw Object.assign(new Error(`FAIL: environment process absent for PID ${expected.pid}`), { code: 'ENVIRONMENT_RACE' });
    if (!row || row.ppid !== expected.ppid || row.pgid !== expected.pgid) throw new Error(`FAIL: environment process identity changed for PID ${expected.pid}`);
    // Only an identical argv with no environment is a hardened-process fallback.
    // Missing rows, changed argv and exposed but malformed HOME never qualify.
    if (row.command === expected.command && unreadable && optionalEnvironment(expected)) {
      unreadable.add(row.pid); continue;
    }
    let environment;
    if (row.command.startsWith(expected.command + ' ')) environment = row.command.slice(expected.command.length + 1);
    else if (row.command.endsWith(' ' + expected.command)) environment = row.command.slice(0, -expected.command.length - 1);
    else throw Object.assign(new Error(`FAIL: ps -E did not expose environment with unchanged argv for PID ${expected.pid}`), { code: 'ENVIRONMENT_RACE' });
    const matches = [...environment.matchAll(/(?:^|\s)HOME=(.*?)(?=\s[A-Za-z_][A-Za-z0-9_]*=|$)/g)];
    if (matches.length !== 1 || !matches[0][1]) throw new Error(`FAIL: ps -E HOME missing or ambiguous for PID ${expected.pid}`);
    homes.set(row.pid, matches[0][1]);
  }
  return homes;
}
// Only Node argv/absent-row races qualify. Command failures, identity changes
// and exposed malformed HOME remain fatal. Never reuse a HOME from a prior pair.
export async function retryNodeHomes(raw, targets, { run = capture, unreadable = new Set(), nodeTargets = [],
  processes = targets, observations = [], now = () => performance.now(),
  delay = ms => new Promise(resolveDelay => setTimeout(resolveDelay, ms)) } = {}) {
  const homes = new Map(), exited = new Set();
  for (const original of targets) {
    let current = original, error, retries = 0;
    const read = value => {
      try {
        const home = processHomes(value, [current], { unreadable }).get(original.pid);
        if (home !== undefined) homes.set(original.pid, home);
        return true;
      }
      catch (failure) { if (failure.code !== 'ENVIRONMENT_RACE' || !nodeTargets.includes(original)) throw failure; error = failure; return false; }
    };
    const recordExit = source => {
      exited.add(original.pid);
      observations.push({ pid: original.pid, ppid: original.ppid, pgid: original.pgid,
        environment: 'exited-before-environment', retries, source });
      const index = processes.findIndex(row => row.pid === original.pid);
      processes[index] = { ...current, executableObservation: { status: 'exited', source } };
    };
    if (read(raw)) continue;
    const deadline = now() + 500;
    const probe = async args => {
      const remaining = Math.floor(deadline - now());
      if (remaining <= 0) throw error;
      const value = await run('ps', args, { timeout: remaining, killSignal: 'SIGKILL' });
      if (now() >= deadline) throw error;
      return value;
    };
    for (; retries < 3; ) {
      await delay(Math.min(50, Math.max(0, deadline - now())));
      retries++;
      const rows = processRows(await probe(['-ww', '-p', String(original.pid), '-o', 'pid=,ppid=,pgid=,args=']));
      if (!rows.length) {
        recordExit('empty targeted ps args snapshot');
        break;
      }
      if (rows.length !== 1 || rows[0].pid !== original.pid || rows[0].ppid !== original.ppid || rows[0].pgid !== original.pgid)
        throw new Error(`FAIL: environment process identity changed for PID ${original.pid}`);
      current = { ...original, command: rows[0].command };
      processes[processes.findIndex(row => row.pid === original.pid)] = current;
      const environment = await probe(['-ww', '-E', '-p', String(original.pid), '-o', 'pid=,ppid=,pgid=,args=']);
      // The PID can exit after fresh args, including on the last retry. capture
      // accepts empty targeted ps only on success or clean exit 1, never timeout.
      if (!environment.trim()) { recordExit('empty targeted ps -E snapshot'); break; }
      if (read(environment)) {
        observations.push({ pid: original.pid, ppid: original.ppid, pgid: original.pgid, retries,
          argvRebound: current.command !== original.command, source: 'fresh ps args + ps -E; PID/PPID/PGID and latest argv matched' });
        break;
      }
    }
    if (!homes.has(original.pid) && !exited.has(original.pid)) throw error;
  }
  return { homes, exited };
}
// Chrome identity is rebuilt from the current snapshot; helper titles may change.
// No cached browser approval or Chrome HOME is evidence for a current chain.
export function forbiddenProcesses(processes, { playwrightRoots = [], browserTmpRoots = [], browserEvidence = [],
  runtimeRoots = [], processHOME = new Map(), operatorHome = userInfo().homedir, deferEnvironment = false,
  unreadable = new Set(), alternativeEvidence = [], readOnlySystemChecks = [], updaterRealpath = realpathSync,
  sofficeRealpath = realpathSync } = {}) {
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
      // R14: the operator's own installed updater, at its canonical path with an
      // exact --wake / --wake-all argv, is Chrome's routine wake regardless of who
      // started it (live browser, a browser that is exiting, or launchd adoption).
      // Any other updater argv still FAILs.
      const proof = { ...candidate, chromeAncestorPid: currentMainChrome.has(row.ppid) ? row.ppid
        : parent?.chromeAncestorPid ?? null };
      allowedUpdaters.set(row.pid, proof); changed = true;
      evidence(row, { alternative: 'canonical installed user updater; exact wake argv (R14: parent not required)',
        executable: proof.executable, realpath: proof.executable, argv: [proof.executable, proof.mode],
        parentPid: row.ppid, chromeAncestorPid: proof.chromeAncestorPid });
    }
  } while (changed);
  return processes.filter(row => {
    const observation = row.executableObservation, paths = executablePaths(row);
    if (inactiveProcess(row)) return false;
    if (unreadableCommand(row.command) && (observation?.status !== 'resolved' || !paths.length)) return true;
    const names = paths.map(path => basename(path));
    if (unreadableCommand(row.command)) names.push(row.command.slice(1, -1));
    if (/(?:^|\s|[;&|])(?:[^\s;|]*\/)?(?:open|xdg-open|osascript)(?=\s|$|[;&|])/.test(row.command)
      || names.some(name => ['open', 'xdg-open', 'osascript'].includes(name))) return true;
    // A hidden child cannot inherit a browser exemption by ancestry alone.
    const chromeAncestorPid = ancestor(row, currentMainChrome);
    const hiddenChromeHelper = unreadableCommand(row.command) && chromeAncestorPid
      && paths.length === 2 && paths.every(path => path.startsWith('/Applications/Google Chrome.app/Contents/'));
    if (unreadableCommand(row.command) && chromeAncestorPid && !hiddenChromeHelper && !allowedUpdaters.has(row.pid)) return true;
    if (developerTool.test(row.command) || names.some(name => ['xcodebuild', 'xcrun'].includes(name))) {
      // ps supplies flattened args: demand the exact three-element spelling,
      // no shell wrapper, extra switches, alternate bundle, or cached ancestry.
      const source = ancestor(row, servers) ?? ancestor(row, gitAncestors);
      const home = servers.get(source) ?? gitAncestors.get(source);
      if (row.command !== licenseArgv.join(' ') || !source || !homeMatches(row, home)) return true;
      if (!deferEnvironment) readOnlySystemChecks.push({ ...row, argv: [...licenseArgv], ancestorPid: source,
        HOME: processHOME.get(row.pid), reason: 'read-only Xcode license check; exact argv and current owned service ancestry' });
      return false;
    }
    if ([row.command, ...paths].some(value => updater.test(value))) {
      return !allowedUpdaters.has(row.pid);
    }
    if ([row.command, ...paths].some(value => soffice.test(value))) {
      const executable = unreadableCommand(row.command) ? observation.path
        : row.command === sofficeExecutable || row.command.startsWith(sofficeExecutable + ' ') ? sofficeExecutable : undefined;
      try { if (executable !== sofficeExecutable || sofficeRealpath(executable) !== sofficeExecutable) return true; }
      catch { return true; }
      const server = ancestor(row, servers);
      const allowed = server && homeMatches(row, servers.get(server));
      if (allowed) evidence(row, { alternative: 'current tested server ancestor with verified isolated HOME; officePreview execFile inherits env',
        executable, realpath: sofficeExecutable, serverAncestorPid: server, inheritedHOME: servers.get(server),
        source: 'ui/server/services/officePreview.js:235-240,411-415' });
      return !allowed;
    }
    if (hiddenChromeHelper) {
      if (!deferEnvironment) browserEvidence.push({ pid: row.pid, environment: 'not-checked',
        source: 'current Chrome ancestor and resolved canonical bundle executable; argv unreadable',
        executable: observation.path, realpath: observation.realpath, chromeAncestorPid,
        userDataDir: profilesByPid.get(chromeAncestorPid) });
      return false;
    }
    if (![row.command, ...paths].some(value => value.includes('/Applications/'))) return false;
    if (currentMainChrome.has(row.pid) || checkedHelpers.has(row.pid)) return false;
    return true;
  });
}
export async function startTCPAudit({ logFile, rootPid = process.pid, groupRoots = () => [], onFatal,
  playwrightRoots = () => [], browserTmpRoots = () => [], runtimeRoots = () => [],
  nodeRoots = () => [],
  updaterRealpath = realpathSync, sofficeRealpath = realpathSync, executableRealpath = realpathSync,
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
      const processes = await resolveUnreadableProcesses(
        processTree(await run('ps', ['-ww', '-axo', 'pid=,ppid=,pgid=,args=']), rootPid, [...knownGroups]), { run, executableRealpath });
      for (const row of processes) if (row.pid !== rootPid && row.pid === row.pgid) knownGroups.add(row.pgid);
      const options = { playwrightRoots: playwrightRoots(), browserTmpRoots: browserTmpRoots(), runtimeRoots: runtimeRoots(), nodeRoots: nodeRoots(), updaterRealpath, sofficeRealpath };
      // Structural violations terminate before environment ps or TCP lsof. The
      // preliminary pass and final pass both require current browser ancestry.
      let forbiddenApps = forbiddenProcesses(processes, { ...options, deferEnvironment: true });
      let homes = new Map();
      const unreadable = new Set(), alternativeEvidence = [];
      const readOnlySystemChecks = [];
      const browserEvidence = [], environmentObservations = [];
      if (!forbiddenApps.length) {
        const targets = environmentTargets(processes, options).filter(row => row.pid !== rootPid);
        const nodeTargets = targets.filter(row => nodeProcess(row) || options.nodeRoots.some(root => root.pid === row.pid));
        let exited = new Set();
        if (targets.length) ({ homes, exited } = await retryNodeHomes(
          await run('ps', ['-ww', '-E', '-p', targets.map(row => row.pid).join(','), '-o', 'pid=,ppid=,pgid=,args=']),
          targets, { run, unreadable, nodeTargets, processes, observations: environmentObservations }));
        // Directly spawned Node roots and all observed Node descendants must
        // expose HOME and match their own mode/mock root, never the operator.
        const roots = new Map([...options.runtimeRoots, ...options.nodeRoots].map(row => [row.pid, row.home]));
        const byPid = new Map(processes.map(row => [row.pid, row]));
        for (const row of nodeTargets.filter(row => !exited.has(row.pid))) {
          const seen = new Set(); let parent = row.pid;
          while (!roots.has(parent) && byPid.has(parent) && !seen.has(parent)) { seen.add(parent); parent = byPid.get(parent).ppid; }
          const home = roots.get(parent);
          if (!home || home === userInfo().homedir || homes.get(row.pid) !== home) throw new Error(`FAIL: Node process ${row.pid} HOME missing or outside isolated service root`);
        }
        forbiddenApps = forbiddenProcesses(processes, { ...options, processHOME: homes, unreadable, alternativeEvidence, readOnlySystemChecks, browserEvidence });
      }
      const environmentEvidence = [...homes].map(([pid, HOME]) => ({ pid, HOME, environment: 'readable', source: 'ps -ww -E; identity and argv matched' }));
      for (const observation of environmentObservations) {
        const proof = environmentEvidence.find(row => row.pid === observation.pid);
        if (proof) Object.assign(proof, observation); else environmentEvidence.push(observation);
      }
      for (const pid of unreadable) environmentEvidence.push(alternativeEvidence.find(row => row.pid === pid)
        ?? { pid, environment: 'unreadable', alternative: 'none; structural authorization failed' });
      // Fail before lsof and evidence I/O, even if either subsequently hangs.
      if (forbiddenApps.length) {
        fail(new Error(`FAIL: runner forbidden process: ${forbiddenApps.map(row => `${row.pid} ${row.command}`).join(', ')}`));
        await record({ timestamp: new Date().toISOString(), phase, runnerPid: rootPid, processes, environmentEvidence, browserEvidence, readOnlySystemChecks, status: 'FAIL', forbiddenProcesses: forbiddenApps });
        return;
      }
      const raw = await run('lsof', ['-nP', '-a', '-p', processes.filter(row => !inactiveProcess(row)).map(row => row.pid).join(','), '-iTCP', '-F', 'pftnT']);
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
