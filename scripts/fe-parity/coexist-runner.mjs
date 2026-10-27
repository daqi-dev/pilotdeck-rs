// #927 F3 coexistence orchestration: assemble the deployment copy once, then
// run each phase as its own start-built runtime + real Chrome pass. The
// coexist suite has its own denominator (the F3 matrix below); it never credits
// the baseline 72.
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { baseline, root } from './env.mjs';
import { prepareRealOSDirectories, realOSPaths, realChromeEnv } from './execution-env.mjs';
import { prepareRealGitBin } from './real-git.mjs';
import { chromeDefaultsEvidence } from './real-browser.mjs';
import { startIsolatedMockLLM } from './real-mock.mjs';
import { reportCases } from './accounting.mjs';
import { assertCanonicalAncestor, portOccupied, startRuntime, waitReady, withRealAudit, realExecutionError } from './real-runner.mjs';
import { DEPLOY_CELLS } from './deploy-matrix.mjs';
import { assembleCoexistDeployment, coexistAllowedPorts, coexistExecutionEnv,
  coexistOrigin, coexistPorts, coexistRunRoot, PHASES } from './coexist-deploy.mjs';

export const coexistTestDir = resolve(root, 'scripts/fe-parity/coexist-tests');
// The F3 matrix. Titles are the identity; drift between this catalog and the
// spec's registrations fails accounting before any browser credit.
export const COEXIST_CASES = Object.freeze({
  valid: Object.freeze([
    'S1 开关开：GET /settings 新 bundle、GET /settingsX 旧 bundle（HTTP 严格段边界）',
    'N1 旧→新 navigate()：sidebar Settings 硬跳新 bundle',
    'N3 前进/后退跨壳遍历：新壳侧出发 goBack 落旧壳',
    'N3b 前进入新壳（特征化）：旧壳重载后其启动导航清除跨壳前进项',
    'N4 新→旧 「返回旧壳」链接硬跳',
    'N5 新→旧 history 原语被拦截硬跳',
    'C1 登录态互通（auth-token 跨壳）',
    'C2a 旧壳 composer 草稿跨壳保留',
    'C2b 探针 composer 草稿跨壳保留',
    'C3 共享键跨跳转不变',
    'D1 /ws 跨壳往返重连（D1 对账）',
  ]),
  invalid: Object.freeze(['N2 旧→新 <Link>：ModelConfigurationErrorScreen 硬跳新 bundle']),
  rollback: Object.freeze([
    'R1 rollback：GET /settings 旧 bundle 照常服务',
    'R2 rollback：旧壳 Settings 导航落在旧 bundle',
  ]),
});
const COEXIST_RUNS = () => [
  { phase: 'valid', label: 'coexist-valid', config: 'coexist.config.mjs', frontend: 'old', report: 'coexist-valid.json', testDir: coexistTestDir },
  { phase: 'invalid', label: 'coexist-invalid', config: 'coexist.config.mjs', frontend: 'old', report: 'coexist-invalid.json', testDir: coexistTestDir },
  { phase: 'rollback', label: 'coexist-rollback', config: 'coexist.config.mjs', frontend: 'old', report: 'coexist-rollback.json', testDir: coexistTestDir },
];
const NATIVE_RUN = { phase: 'valid', label: 'native-auth-new', config: 'coexist-native.config.mjs', frontend: 'new', report: 'new-native-auth.json', testDir: resolve(baseline, 'e2e') };

export async function assertCoexistPortsFree(check = portOccupied) {
  for (const port of coexistAllowedPorts) if (await check(port)) throw new Error(`FAIL: coexist required 127.0.0.1:${port} occupied; no process was killed`);
}
async function waitCoexistReady(runtime, env, isInterrupted, audit, phase) {
  if (phase !== 'invalid') return waitReady(runtime, env, isInterrupted, audit, { ports: coexistPorts, origin: coexistOrigin });
  // The invalid phase intentionally never starts a gateway (the supervisor
  // stops it for configuration=invalid), so readiness is the server itself.
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    audit.assertSafe();
    if (isInterrupted()) throw new Error('Run interrupted');
    runtime.assertAlive();
    try {
      const [front, auth] = await Promise.all([
        fetch(coexistOrigin, { signal: AbortSignal.timeout(1000) }),
        fetch(`http://127.0.0.1:${coexistPorts.server}/api/auth/status`, { signal: AbortSignal.timeout(1000) }),
      ]);
      const status = auth.ok ? await auth.json() : null;
      await front.body?.cancel();
      if (front.ok && typeof status?.needsSetup === 'boolean') return;
    } catch { /* keep polling */ }
    await new Promise(done => setTimeout(done, 250));
  }
  throw new Error(`Timed out waiting for coexist ${phase} runtime (server); see ${runtime.log}`);
}
export function coexistAccounting(reports, runs) {
  const rows = [], errors = [];
  for (const run of runs) {
    const titles = run.frontend === 'new' ? null : COEXIST_CASES[run.phase];
    const report = reports[run.label];
    const tests = report ? reportCases(report, run.testDir) : [];
    if (titles) {
      const executed = tests.filter(row => !row.results.every(result => result.status === 'skipped')).map(row => row.title);
      if (report && JSON.stringify([...executed].sort()) !== JSON.stringify([...titles].sort())) {
        errors.push(`coexist ${run.phase} registration drift: ${JSON.stringify(executed)}`);
      }
      for (const title of titles) {
        const row = tests.find(item => item.title === title);
        const results = row?.results ?? [];
        const status = !row ? 'NOT_RUN' : results.length && results.every(result => result.status === 'passed') ? 'PASS' : 'FAIL';
        rows.push({ phase: run.phase, title, status, reason: row ? null : 'no result in the phase report' });
      }
    } else {
      // The native pairing run executes a frozen baseline spec verbatim; its
      // original result is the evidence, whatever it says.
      for (const row of tests) {
        const results = row.results ?? [];
        const status = results.length && results.every(result => result.status === 'passed') ? 'PASS' : 'FAIL';
        rows.push({ phase: 'new-native-auth', title: `${row.file} :: ${row.title}`, status, reason: null });
      }
      if (!tests.length) rows.push({ phase: 'new-native-auth',
        title: 'real-auth-native.spec.mjs :: native Settings reads, saves, reloads, and authenticates runtime/config calls',
        status: 'NOT_RUN', reason: 'no result in the native pairing report' });
    }
  }
  const status = rows.length && rows.every(row => row.status === 'PASS') ? 'PASS' : 'FAIL';
  return { rows, status, errors, executed: rows.filter(row => row.status === 'PASS').length, expected: rows.length };
}
// B-class new-side pairing matrix: only the native-auth cell executes (against
// this package's coexistence deployment); every other cell is recorded
// NOT_RUN_NEW_SIDE, never silently skipped. No scoring change: the new side can
// never turn the B suite green.
export function newSideAccounting(nativeReport, executed) {
  const rows = DEPLOY_CELLS.map(cell => cell.id === 'native-auth'
    ? { cell: cell.id, status: nativeReport?.rows.some(row => row.status === 'PASS') ? 'PASS' : 'FAIL',
        reason: 'executed against the #927 coexistence deployment (real native TS, switch on); the probe has no settings pages, an honest FAIL is expected until F3+ migration' }
    : { cell: cell.id, status: 'NOT_RUN_NEW_SIDE',
        reason: '目标路由未迁移：新侧只接 native-auth 配对；其余 profile 的 vite 内存生成模块不适用于静态共存部署' });
  return { rows, executed, status: 'FAIL' };
}
async function runCoexistRuns({ output, command, children, isInterrupted, runs }) {
  const reports = {}, executions = [];
  let sessionRoot, mockStats = null;
  const attempted = await withRealAudit({ output, children, allowedPorts: coexistAllowedPorts }, async audit => {
    await assertCanonicalAncestor(coexistRunRoot);
    await mkdir(coexistRunRoot, { recursive: true });
    sessionRoot = await mkdtemp(resolve(coexistRunRoot, 'run-'));
    const envs = {};
    for (const run of runs) {
      audit.assertSafe();
      const env = coexistExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, run.label), phase: run.phase, frontend: run.frontend });
      await prepareRealOSDirectories(env);
      envs[run.label] = env;
      const git = await prepareRealGitBin(env);
      await writeFile(resolve(output, `${run.label}.isolation.json`), JSON.stringify({
        phase: run.phase, frontend: run.frontend, baseURL: coexistOrigin, ports: coexistPorts,
        runDir: env.PILOTDECK_E2E_RUN_DIR, ...realOSPaths(env.PILOTDECK_E2E_RUN_DIR), PILOT_HOME: env.PILOT_HOME,
        PILOTDECK_NEW_SHELL: env.PILOTDECK_NEW_SHELL ?? null,
        PILOTDECK_SKIP_BROWSER_OPEN: env.PILOTDECK_SKIP_BROWSER_OPEN, SKIP_BROWSER_OPEN: env.SKIP_BROWSER_OPEN, git,
        browser: { HOME: realChromeEnv(env).HOME, TMPDIR: env.TMPDIR, profile: 'Playwright temporary playwright_chromiumdev_profile-*', ...chromeDefaultsEvidence() },
      }, null, 2) + '\n', { flag: 'wx' });
    }
    await assertCoexistPortsFree();
    await audit.checkpoint('before-coexist-runtime');
    const mockEnv = coexistExecutionEnv(process.env, { output, runDir: resolve(sessionRoot, 'mock'), phase: 'valid' });
    await prepareRealOSDirectories(mockEnv);
    const mock = await startIsolatedMockLLM(mockEnv, children);
    mockStats = mock.stats;
    try {
      for (const run of runs) {
        audit.assertSafe();
        if (isInterrupted()) throw new Error('Run interrupted');
        for (const port of Object.values(coexistPorts)) if (await portOccupied(port)) throw new Error(`coexist port became occupied: ${port}`);
        const runtime = await startRuntime(envs[run.label], resolve(output, `${run.label}.runtime.log`), children, 'coexist-launch.mjs');
        try {
          console.log(`Starting coexist ${run.label}: ${coexistOrigin}`);
          await waitCoexistReady(runtime, envs[run.label], isInterrupted, audit, run.phase);
          await audit.checkpoint(`before-${run.label}-playwright`);
          const execution = await command(['test', '--config', resolve(root, 'scripts/fe-parity', run.config), '--forbid-only'], envs[run.label]);
          await writeFile(resolve(output, `${run.label}.stdout.log`), execution.stdout);
          await writeFile(resolve(output, `${run.label}.stderr.log`), execution.stderr);
          let report;
          try { report = JSON.parse(await readFile(resolve(output, run.report), 'utf8')); }
          catch { report = { errors: [{ message: `No coexist JSON result; exit ${execution.code}` }] }; }
          const error = realExecutionError(execution, report, coexistTestDir);
          if (error) (report.errors ||= []).push({ message: error });
          reports[run.label] = report;
          executions.push({ run: run.label, exitCode: execution.code, signal: execution.signal, report: run.report });
          await audit.checkpoint(`after-${run.label}-playwright`);
        } finally { await runtime.close(); await audit.checkpoint(`after-${run.label}-runtime`); }
      }
    } finally { await mock.close(); }
    return { accounting: coexistAccounting(reports, runs), executions, runtimeRoot: sessionRoot, mock: mock.stats };
  });
  return { ...attempted, reports, audit: { status: 'PASS', log: 'tcp-connections.jsonl',
    scope: 'process tree + fixed coexist loopback ports; periodic ps/lsof sampling' } };
}
export async function runCoexist({ output, command, children, isInterrupted, phases = PHASES }) {
  if (phases.some(phase => !PHASES.includes(phase)) || new Set(phases).size !== phases.length) throw new Error('Invalid coexist phases');
  // Assembly is outside the audit: it neither listens nor spawns.
  await assembleCoexistDeployment(output);
  const runs = COEXIST_RUNS().filter(run => phases.includes(run.phase));
  try {
    return await runCoexistRuns({ output, command, children, isInterrupted, runs });
  } catch (error) {
    // Preserve any original phase evidence even on a fatal audit error.
    const reports = {};
    for (const run of runs) {
      try { reports[run.label] = JSON.parse(await readFile(resolve(output, run.report), 'utf8')); } catch { /* Missing remains NOT_RUN. */ }
    }
    await writeFile(resolve(output, 'coexist-fatal.log'), (error.stack || error.message) + '\n');
    const accounting = coexistAccounting(reports, runs);
    accounting.errors.push(`coexist fatal audit/orchestration error: ${error.message}`);
    accounting.status = 'FAIL';
    return { accounting, executions: [], reports, fatalError: error.stack || error.message,
      audit: { status: 'FAIL', log: 'tcp-connections.jsonl', reason: 'Coexist phases did not complete with a successful final audit' } };
  }
}
// B-class new-side pairing: the single native-auth cell against the coexistence
// deployment. Old cells stay untouched; the new side never changes B scoring.
export async function runCoexistNativeAuth({ output, command, children, isInterrupted }) {
  await assembleCoexistDeployment(output);
  try {
    const result = await runCoexistRuns({ output, command, children, isInterrupted, runs: [NATIVE_RUN] });
    return { ...result, newSide: newSideAccounting(result.accounting, result.executions.length > 0) };
  } catch (error) {
    await writeFile(resolve(output, 'coexist-fatal.log'), (error.stack || error.message) + '\n');
    return { accounting: { rows: [], status: 'FAIL', errors: [`coexist native pairing fatal error: ${error.message}`] },
      executions: [], newSide: newSideAccounting(null, false), fatalError: error.stack || error.message,
      audit: { status: 'FAIL', log: 'tcp-connections.jsonl', reason: 'Native pairing did not complete with a successful final audit' } };
  }
}
