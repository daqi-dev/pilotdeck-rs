import { mkdir, mkdtemp, writeFile, readFile, realpath } from 'node:fs/promises';
import { resolve } from 'node:path';
import { baseline, root } from './env.mjs';
import { prepareRealOSDirectories, realOSPaths, realChromeEnv } from './execution-env.mjs';
import { prepareRealGitBin } from './real-git.mjs';
import { chromeDefaultsEvidence } from './real-browser.mjs';
import { startIsolatedMockLLM } from './real-mock.mjs';
import { reportCases, validateDeployRegistration, reconcileDeploy } from './accounting.mjs';
import { assertCanonicalAncestor, portOccupied, startRuntime, waitReady, withRealAudit, realExecutionError } from './real-runner.mjs';
import { deployExecutionEnv, deployAllowedPorts, deployRunRoot, deployOrigin, deployPorts } from './deploy-matrix.mjs';

export async function assertDeployPortsFree(check = portOccupied) {
  for (const port of deployAllowedPorts) if (await check(port)) throw new Error(`FAIL: B required 127.0.0.1:${port} occupied; no process was killed`);
}
// Keep the exact fetch/Response path callable by the offline check. Playwright
// APIResponse.ok() is a method; Node fetch returns a Response.ok property.
export async function assertReplacementReady(fetchImpl = globalThis.fetch) {
  const health = await fetchImpl(`http://127.0.0.1:${deployPorts.knowledge}/healthz`, { signal: AbortSignal.timeout(5000) });
  if (!health.ok) {
    await health.body?.cancel();
    throw new Error(`Replacement conformance process not ready: HTTP ${health.status}`);
  }
  const body = await health.json();
  if (body?.implementationId !== 'replacement.knowledge') throw new Error('Replacement conformance process not ready: implementationId mismatch');
}
export function failedDeployment({ inventory, reports, testDir, cells, error, executions, runtimeRoot, mock }) {
  const accounting = reconcileDeploy(inventory, reports, testDir, cells);
  accounting.errors.push(`B fatal audit/orchestration error: ${error.message}`);
  accounting.status = 'FAIL';
  return { accounting, executions, runtimeRoot, mock, fatalError: error.stack || error.message,
    audit: { status: 'FAIL', log: 'tcp-connections.jsonl', reason: 'Matrix did not complete with a successful final audit' } };
}
export async function runDeploy({ output, inventory, cells, command, children, isInterrupted }) {
  const reports = {}, executions = [], testDir = resolve(baseline, 'e2e');
  let sessionRoot, mockStats = null;
  try {
    const result = await withRealAudit({ output, children, allowedPorts: deployAllowedPorts }, async audit => {
      await assertCanonicalAncestor(deployRunRoot);
      await mkdir(deployRunRoot, { recursive: true });
      if (await realpath(output) !== output) throw new Error('B output symlink escape');
      sessionRoot = await mkdtemp(resolve(deployRunRoot, 'run-'));
      const config = resolve(root, 'scripts/fe-parity/deploy.config.mjs'), envs = {};
      // Verify every requested cell's original registrations before any listen.
      // Even BLOCKED cells are listed, so missing registrations cannot hide gaps.
      for (const cell of cells) {
        audit.assertSafe();
        const env = deployExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, cell.id), cell });
        await prepareRealOSDirectories(env);
        envs[cell.id] = env;
        const list = await command(['test', '--config', config, '--list', '--forbid-only', '--reporter=json'], env);
        await writeFile(resolve(output, `old-${cell.id}.inventory.json`), list.stdout);
        await writeFile(resolve(output, `old-${cell.id}.inventory.stderr.log`), list.stderr);
        if (list.code !== 0 || list.signal) throw new Error(`${cell.id}: B registration failed: ${list.stderr}`);
        const report = JSON.parse(list.stdout);
        if (report.errors?.length) throw new Error(`${cell.id}: B registration errors`);
        validateDeployRegistration(reportCases(report, testDir), inventory, cell);
        if (cell.blocked) {
          executions.push({ cell: cell.id, status: 'BLOCKED', reason: cell.blocked });
          continue;
        }
        const git = await prepareRealGitBin(env);
        await writeFile(resolve(output, `old-${cell.id}.isolation.json`), JSON.stringify({
          cell: cell.id, profile: cell.profile, baseURL: deployOrigin, ports: deployPorts,
          runDir: env.PILOTDECK_E2E_RUN_DIR, ...realOSPaths(env.PILOTDECK_E2E_RUN_DIR), PILOT_HOME: env.PILOT_HOME,
          gateEnv: Object.fromEntries(Object.entries(env).filter(([name]) => /^(FORMAL_|REAL_(?:COMPOSITION|NATIVE)|REPLACEMENT_)/.test(name))),
          PILOTDECK_SKIP_BROWSER_OPEN: env.PILOTDECK_SKIP_BROWSER_OPEN, SKIP_BROWSER_OPEN: env.SKIP_BROWSER_OPEN, git,
          browser: { HOME: realChromeEnv(env).HOME, TMPDIR: env.TMPDIR, profile: 'Playwright temporary playwright_chromiumdev_profile-*', ...chromeDefaultsEvidence() },
        }, null, 2) + '\n', { flag: 'wx' });
      }
      await assertDeployPortsFree();
      await audit.checkpoint('before-B-runtime');
      const mockEnv = deployExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, 'mock'), cell: cells.find(cell => cell.id === 'native-auth') });
      await prepareRealOSDirectories(mockEnv);
      await writeFile(resolve(output, 'mock.isolation.json'), JSON.stringify({ role: 'mock Node', ...realOSPaths(mockEnv.PILOTDECK_E2E_RUN_DIR),
        PILOT_HOME: mockEnv.PILOT_HOME, SKIP_BROWSER_OPEN: mockEnv.SKIP_BROWSER_OPEN }, null, 2) + '\n', { flag: 'wx' });
      const mock = await startIsolatedMockLLM(mockEnv, children);
      mockStats = mock.stats;
      try {
        for (const cell of cells.filter(cell => !cell.blocked)) {
          audit.assertSafe();
          if (isInterrupted()) throw new Error('Run interrupted');
          for (const port of Object.values(deployPorts)) if (await portOccupied(port)) throw new Error(`B port became occupied: ${port}`);
          let runtime;
          try {
            runtime = await startRuntime(envs[cell.id], resolve(output, `old-${cell.id}.runtime.log`), children, 'deploy-launch.mjs');
            console.log(`Starting B ${cell.id}: ${deployOrigin}`);
            await waitReady(runtime, envs[cell.id], isInterrupted, audit, { ports: deployPorts, origin: deployOrigin });
            if (cell.profile === 'replacement') await assertReplacementReady();
            await audit.checkpoint(`before-B-${cell.id}-playwright`);
            const execution = await command(['test', '--config', config, '--forbid-only'], envs[cell.id]);
            await writeFile(resolve(output, `old-${cell.id}.stdout.log`), execution.stdout);
            await writeFile(resolve(output, `old-${cell.id}.stderr.log`), execution.stderr);
            let report;
            try { report = JSON.parse(await readFile(resolve(output, `old-${cell.id}.json`), 'utf8')); }
            catch { report = { errors: [{ message: `No B JSON result; exit ${execution.code}` }] }; }
            const error = realExecutionError(execution, report, testDir);
            if (error) (report.errors ||= []).push({ message: error });
            reports[cell.id] = report;
            executions.push({ cell: cell.id, exitCode: execution.code, signal: execution.signal, report: `old-${cell.id}.json` });
            await audit.checkpoint(`after-B-${cell.id}-playwright`);
          } catch (error) {
            audit.assertSafe(); // A safety failure aborts the matrix immediately.
            reports[cell.id] = { errors: [{ message: error.message }], suites: [] };
            executions.push({ cell: cell.id, error: error.message });
            await writeFile(resolve(output, `old-${cell.id}.orchestration-error.log`), error.stack + '\n');
          } finally {
            await runtime?.close();
            await audit.checkpoint(`after-B-${cell.id}-runtime`);
          }
        }
      } finally { await mock.close(); }
      const accounting = reconcileDeploy(inventory, reports, testDir, cells);
      if (mock.stats.errors.length || mock.stats.rejected) {
        accounting.errors.push(`B mock LLM rejected ${mock.stats.rejected} requests: ${mock.stats.errors.join('; ')}`);
        accounting.status = 'FAIL';
      }
      return { accounting, executions, runtimeRoot: sessionRoot, mock: mock.stats };
    });
    // Assigned only after the final after-cleanup audit succeeds.
    return { ...result, audit: { status: 'PASS', log: 'tcp-connections.jsonl', scope: 'process tree + fixed B loopback ports; periodic ps/lsof sampling' } };
  } catch (error) {
    // Preserve original execution evidence even if a later safety/cleanup gate
    // fails. Never report zero executions merely because the audit was fatal.
    for (const cell of cells.filter(cell => !cell.blocked && !reports[cell.id])) {
      try { reports[cell.id] = JSON.parse(await readFile(resolve(output, `old-${cell.id}.json`), 'utf8')); } catch { /* Missing original remains NOT_RUN. */ }
    }
    await writeFile(resolve(output, 'B-fatal.log'), (error.stack || error.message) + '\n');
    return failedDeployment({ inventory, reports, testDir, cells, error, executions, runtimeRoot: sessionRoot, mock: mockStats });
  }
}
