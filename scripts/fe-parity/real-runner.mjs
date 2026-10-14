import net from 'node:net';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, writeFile, readFile, realpath, open } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { baseline, root, realPorts, realOrigin, realRunRoot } from './env.mjs';
import { realExecutionEnv, assertRealExecutionEnv, prepareRealOSDirectories, realOSPaths, realChromeEnv } from './execution-env.mjs';
import { caseId, reportCases, reconcileReal } from './accounting.mjs';
import { startIsolatedMockLLM } from './real-mock.mjs';
import { chromeDefaultsEvidence } from './real-browser.mjs';
import { startTCPAudit } from './real-tcp-audit.mjs';
import { prepareRealGitBin, assertRealGitBin } from './real-git.mjs';

// Validate the nearest existing ancestor before mkdir, so symlinked missing
// descendants cannot create runtime/output files outside the worktree.
export async function assertCanonicalAncestor(path) {
  for (let current = path; ; current = dirname(current)) {
    try {
      if (await realpath(current) !== current) throw new Error('Real path has a symlink escape');
      return;
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      if (dirname(current) === current) throw error;
    }
  }
}

// A successful connect means occupied. Only ECONNREFUSED establishes idle;
// permissions/timeouts/other errors are failures, never permission to launch.
export function portOccupied(port) {
  return new Promise((done, reject) => {
    const socket = net.createConnection({ host: '127.0.0.1', port });
    socket.setTimeout(1000);
    socket.once('connect', () => { socket.destroy(); done(true); });
    socket.once('error', error => { socket.destroy(); if (error.code === 'ECONNREFUSED') done(false); else reject(error); });
    socket.once('timeout', () => { socket.destroy(); reject(new Error(`Port check timeout ${port}`)); });
  });
}
export async function assertRealPortsFree(check = portOccupied) {
  for (const port of [realPorts.mock, ...Object.values(realPorts.ready), ...Object.values(realPorts.onboarding)]) {
    if (await check(port)) throw new Error(`FAIL: required 127.0.0.1:${port} occupied; no process was killed`);
  }
}
export function validateRealRegistration(rows, inventory, mode) {
  const expected = inventory.filter(row => (row.file === 'real/onboarding.spec.mjs') === (mode === 'onboarding'));
  if (JSON.stringify(rows.map(caseId).sort()) !== JSON.stringify(expected.map(caseId).sort())) throw new Error(`${mode}: D registration identity drift`);
  for (const row of rows) {
    const registration = expected.find(item => caseId(item) === caseId(row));
    const line = registration.line ?? Number(registration.source?.registration.split(':').at(-1));
    if (row.line !== line || !Number.isInteger(row.column) || row.column < 1
      || registration.column !== undefined && row.column !== registration.column) throw new Error(`${mode}: frozen registration location drift ${caseId(row)}`);
  }
  return expected;
}
export async function startRuntime(env, log, children, entrypoint = 'real-launch.mjs') {
  assertRealExecutionEnv(env);
  await assertRealGitBin(env);
  if (!['real-launch.mjs', 'deploy-launch.mjs'].includes(entrypoint)) throw new Error('Unknown owned runtime entrypoint');
  const handle = await open(log, 'wx');
  const child = spawn(process.execPath, [resolve(root, 'scripts/fe-parity', entrypoint)], {
    cwd: root, detached: true, stdio: ['ignore', handle.fd, handle.fd],
    env: { ...env, FE_REAL_RUNTIME: '1', FE_NETWORK_LOG: resolve(env.FE_OUTPUT, 'network.jsonl'),
      NODE_OPTIONS: `--import=${resolve(root, 'scripts/fe-parity/real-network.mjs')}` },
  });
  children.add(child);
  child.feRuntimeHome = env.HOME;
  let spawnError;
  child.once('error', error => { spawnError = error; });
  const exited = new Promise(done => child.once('close', done));
  const signalGroup = signal => { if (!child.pid) return; try { process.kill(-child.pid, signal); } catch (error) { if (error.code !== 'ESRCH') throw error; } };
  return { child, log, assertAlive() {
    if (spawnError) throw spawnError;
    if (child.exitCode !== null || child.signalCode !== null) throw new Error(`Runtime exited early (${child.exitCode ?? child.signalCode}); see ${log}`);
  }, close: async () => {
    // The supervisor and its three non-detached children share this owned group.
    signalGroup('SIGTERM');
    const timer = setTimeout(() => signalGroup('SIGKILL'), 5000);
    try { await exited; } finally { clearTimeout(timer); signalGroup('SIGKILL'); children.delete(child); await handle.close(); }
  } };
}
export function bridgeReady(log, token, env) {
  return Boolean(token.trim()) && log.includes(`[pilotdeck-bridge] connected → ${env.PILOTDECK_GATEWAY_URL}`);
}
export function realExecutionError(execution, report, testDir) {
  const failed = reportCases(report, testDir).some(row => row.results.some(result => ['failed', 'timedOut'].includes(result.status)));
  // Exit 1 is Playwright's normal behavioral-failure exit. Keep every original
  // assertion in accounting; other exits/signals remain orchestration errors.
  if (execution.signal || ![0, 1].includes(execution.code) || (execution.code === 1) !== failed) {
    return `Playwright exit ${execution.code}, signal ${execution.signal || 'none'} inconsistent with original results`;
  }
  return null;
}
export async function waitReady(runtime, env, isInterrupted, audit, deployment) {
  const deadline = Date.now() + 120_000, mode = env.PILOTDECK_E2E_MODE, ports = deployment?.ports || realPorts[mode];
  while (Date.now() < deadline) {
    audit.assertSafe();
    if (isInterrupted()) throw new Error('Run interrupted');
    runtime.assertAlive();
    try {
      const [front, auth] = await Promise.all([
        fetch(deployment?.origin || realOrigin(mode), { signal: AbortSignal.timeout(1000) }),
        fetch(`http://127.0.0.1:${ports.server}/api/auth/status`, { signal: AbortSignal.timeout(1000) }),
      ]);
      const status = auth.ok ? await auth.json() : null;
      await front.body?.cancel();
      if (front.ok && typeof status?.needsSetup === 'boolean') {
        const gateway = await portOccupied(ports.gateway);
        if (mode === 'onboarding' && gateway) throw new Error('Onboarding unexpectedly started a gateway');
        if (mode === 'onboarding') return;
        if (gateway) {
          const [token, log] = await Promise.all([readFile(env.PILOTDECK_GATEWAY_TOKEN_PATH, 'utf8'), readFile(runtime.log, 'utf8')]);
          if (bridgeReady(log, token, env)) return;
        }
      }
    } catch (error) {
      if (error.message === 'Onboarding unexpectedly started a gateway') throw error;
    }
    await new Promise(done => setTimeout(done, 250));
  }
  throw new Error(`Timed out waiting for isolated ${mode} runtime (ready requires token file and native bridge handshake; see ${runtime.log})`);
}
export async function runReal({ output, inventory, modes, command, children, isInterrupted }) {
  return withRealAudit({ output, children }, audit => runRealAudited({ output, inventory, modes, command, children, isInterrupted, audit }));
}
// Both B and D audit from before their first runtime through final cleanup.
export async function withRealAudit({ output, children, allowedPorts }, operation) {
  let killCompletion, audit;
  try {
    audit = await startTCPAudit({ logFile: resolve(output, 'tcp-connections.jsonl'), allowedPorts,
      groupRoots: () => [...children].map(child => child.pid),
      playwrightRoots: () => [...children].filter(child => child.fePlaywright).map(child => child.pid),
      browserTmpRoots: () => [...children].filter(child => child.fePlaywright).map(child => child.feBrowserTmp),
      runtimeRoots: () => [...children].filter(child => child.feRuntimeHome).map(child => ({ pid: child.pid, home: child.feRuntimeHome })),
      nodeRoots: () => [...children].filter(child => child.feServiceHome || child.feRuntimeHome)
        .map(child => ({ pid: child.pid, home: child.feServiceHome || child.feRuntimeHome })),
      onFatal: (error, observedGroups) => {
        console.error(error.message);
        const groups = [...new Set([...observedGroups, ...[...children].map(child => child.pid).filter(Number.isInteger)])];
        const signal = value => { for (const pid of groups) { try { process.kill(-pid, value); } catch (error) { if (error.code !== 'ESRCH') console.error(error.message); } } };
        signal('SIGTERM');
        killCompletion = new Promise(done => setTimeout(() => { signal('SIGKILL'); done(); }, 5000));
      },
    });
    return await operation(audit);
  } finally { try { await audit?.stop(); } finally { await killCompletion; } }
}
async function runRealAudited({ output, inventory, modes, command, children, isInterrupted, audit }) {
  const d = inventory.filter(row => row.file.startsWith('real/')), testDir = resolve(baseline, 'e2e');
  await assertCanonicalAncestor(realRunRoot);
  await mkdir(realRunRoot, { recursive: true });
  if (await realpath(realRunRoot) !== realRunRoot || await realpath(output) !== output) throw new Error('Real run/output path has a symlink escape');
  const sessionRoot = await mkdtemp(resolve(realRunRoot, 'run-'));
  const config = resolve(root, 'scripts/fe-parity/real.config.mjs');
  const envs = {}, executions = [], reports = {};
  // List both partitions before any listen or process spawn.
  for (const mode of modes) {
    audit.assertSafe();
    const env = realExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, mode), mode });
    await prepareRealOSDirectories(env);
    const git = await prepareRealGitBin(env);
    await writeFile(resolve(output, `old-${mode}.isolation.json`), JSON.stringify({
      mode, runDir: env.PILOTDECK_E2E_RUN_DIR, ...realOSPaths(env.PILOTDECK_E2E_RUN_DIR),
      PILOT_HOME: env.PILOT_HOME, PILOTDECK_SKIP_BROWSER_OPEN: env.PILOTDECK_SKIP_BROWSER_OPEN,
      git,
      browser: { HOME: realChromeEnv(env).HOME, TMPDIR: env.TMPDIR, profile: 'Playwright temporary playwright_chromiumdev_profile-*', ...chromeDefaultsEvidence() },
      assertions: 'PASS: OS HOME differs from os.userInfo().homedir; exact paths and canonical ancestors checked',
    }, null, 2) + '\n', { flag: 'wx' });
    envs[mode] = env;
    const list = await command(['test', '--config', config, '--list', '--forbid-only', '--reporter=json'], env);
    await writeFile(resolve(output, `old-${mode}.inventory.json`), list.stdout);
    await writeFile(resolve(output, `old-${mode}.inventory.stderr.log`), list.stderr);
    if (list.code !== 0) throw new Error(`${mode} registration failed: ${list.stderr}`);
    const report = JSON.parse(list.stdout);
    if (report.errors?.length) throw new Error(`${mode} registration errors`);
    validateRealRegistration(reportCases(report, testDir), d, mode);
  }
  await assertRealPortsFree(); // never start on another process's ports
  await audit.checkpoint('before-runtime');
  const mockEnv = realExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, 'mock'), mode: 'ready' });
  await prepareRealOSDirectories(mockEnv);
  await writeFile(resolve(output, 'mock.isolation.json'), JSON.stringify({
    role: 'mock Node', runDir: mockEnv.PILOTDECK_E2E_RUN_DIR, ...realOSPaths(mockEnv.PILOTDECK_E2E_RUN_DIR),
    PILOT_HOME: mockEnv.PILOT_HOME, PILOTDECK_SKIP_BROWSER_OPEN: mockEnv.PILOTDECK_SKIP_BROWSER_OPEN,
    assertions: 'PASS: isolated service environment checked before spawn and at mock entrypoint',
  }, null, 2) + '\n', { flag: 'wx' });
  const mock = await startIsolatedMockLLM(mockEnv, children);
  try {
    for (const mode of modes) {
      audit.assertSafe();
      if (isInterrupted()) throw new Error('Run interrupted');
      // Recheck the deployment ports immediately before spawn, including mode two.
      for (const port of Object.values(realPorts[mode])) if (await portOccupied(port)) throw new Error(`Port became occupied: ${port}`);
      const runtime = await startRuntime(envs[mode], resolve(output, `old-${mode}.runtime.log`), children);
      try {
        console.log(`Starting isolated ${mode} native runtime: ${realOrigin(mode)}`);
        await waitReady(runtime, envs[mode], isInterrupted, audit);
        await audit.checkpoint(`before-${mode}-playwright`);
        const execution = await command(['test', '--config', config, '--forbid-only'], envs[mode]);
        await writeFile(resolve(output, `old-${mode}.stdout.log`), execution.stdout);
        await writeFile(resolve(output, `old-${mode}.stderr.log`), execution.stderr);
        await audit.checkpoint(`after-${mode}-playwright`);
        let report;
        try { report = JSON.parse(await readFile(resolve(output, `old-${mode}.json`), 'utf8')); }
        catch { report = { errors: [{ message: `No JSON result; exit ${execution.code}` }] }; }
        const exitError = realExecutionError(execution, report, testDir);
        if (exitError) (report.errors ||= []).push({ message: exitError });
        reports[mode] = report;
        executions.push({ mode, exitCode: execution.code, signal: execution.signal, report: `old-${mode}.json` });
      } catch (error) {
        audit.assertSafe(); // a safety failure aborts both modes, never continues
        // Keep missing tests NOT_RUN. Still run the independent second deployment.
        reports[mode] = { errors: [{ message: error.message }], suites: [] };
        executions.push({ mode, error: error.message });
        await writeFile(resolve(output, `old-${mode}.orchestration-error.log`), error.stack + '\n');
      } finally { await runtime.close(); await audit.checkpoint(`after-${mode}-runtime`); }
    }
  } finally { await mock.close(); }
  const accounting = reconcileReal(d, reports, testDir, { ready: realOrigin('ready'), onboarding: realOrigin('onboarding') }, modes);
  if (mock.stats.errors.length || mock.stats.rejected) {
    accounting.errors.push(`Mock LLM rejected ${mock.stats.rejected} requests; handler errors: ${mock.stats.errors.join('; ')}`);
    accounting.status = 'FAIL';
    if (accounting.oldGate) {
      accounting.oldGate.errors.push(accounting.errors.at(-1));
      accounting.oldGate.status = 'FAIL';
    }
  }
  return { accounting, executions, runtimeRoot: sessionRoot, mock: mock.stats };
}
