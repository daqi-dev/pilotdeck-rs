#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { baseline, root, requireUI, historicalInventory, ownedPath, origin } from './env.mjs';
import { reportCases, checkInventory, reconcile, reconcilePair, loadReportPair, PENDING_STATUS } from './accounting.mjs';
import { executionEnv, PROFILES } from './execution-env.mjs';
import { verifyBaseline } from './baseline.mjs';
import { selfCheck } from './self-check.mjs';
import { startMock, TOKEN } from './mock.mjs';
import { startFrontend } from './serve.mjs';
const { values } = parseArgs({ options: {
  help: { type: 'boolean' }, 'self-check': { type: 'boolean' }, 'network-self-check': { type: 'boolean' },
  reconcile: { type: 'boolean' }, suite: { type: 'string', default: 'skeleton' },
  frontends: { type: 'string', default: 'old,new' }, 'old-report': { type: 'string', multiple: true }, 'new-report': { type: 'string', multiple: true },
  output: { type: 'string' }, profiles: { type: 'string', default: PROFILES.join(',') }, 'rules-only': { type: 'boolean' },
} });
if (values.help) {
  console.log(`Usage (worktree root; system Chrome only, no browser downloads):
  node scripts/fe-parity/run.mjs --self-check [--rules-only]
  node scripts/fe-parity/run.mjs --network-self-check
  node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old
  node scripts/fe-parity/run.mjs --suite streaming-scroll --frontends old,new
  node scripts/fe-parity/run.mjs --suite baseline --profiles staffdeck,minimal,replacement --frontends old,new
  node scripts/fe-parity/run.mjs --suite skeleton --frontends old,new
  node scripts/fe-parity/run.mjs --reconcile --old-report <JSON> --new-report <JSON>

Baseline config is repository-owned: no webServer/global setup/real launch.
Every profile produces a complete original 72-case JSON; repeated report flags merge profiles.
Streaming and skeleton have separate denominators; they never credit the baseline 72.
Single-side PASS means calibration only. The probe's missing chat is FAIL, never skipped.
--rules-only omits build-artifact checks (self-check only), never used by browser preflight.
Ports 28376/28377/28378; bind 127.0.0.1; artifacts/profiles stay in this worktree.
See README.md for known incomplete mock deployment coverage and coordinator commands.`);
  process.exit(0);
}
const frontends = values.frontends.split(','), profiles = values.profiles.split(',');
if (!['old', 'new', 'old,new'].includes(values.frontends)) throw new Error('frontends must be old, new or old,new');
if (!['skeleton', 'streaming-scroll', 'baseline'].includes(values.suite)) throw new Error('Unknown suite');
if (new Set(profiles).size !== profiles.length || profiles.some(profile => !PROFILES.includes(profile))) throw new Error('Invalid/duplicate profiles');
if ([values['self-check'], values['network-self-check'], values.reconcile].filter(Boolean).length > 1) throw new Error('Select one run mode');
if (values['rules-only'] && !values['self-check']) throw new Error('--rules-only requires --self-check');
if (!values.reconcile && (values['old-report'] || values['new-report'])) throw new Error('Reports require --reconcile');
const base = resolve(root, '_tmp/fe-parity'); await mkdir(base, { recursive: true });
let output;
if (values.output) {
  output = ownedPath(values.output);
  await mkdir(dirname(output), { recursive: true });
  await mkdir(output); // EEXIST: never overwrite a previous run or its evidence.
} else output = await mkdtemp(resolve(base, 'run-'));
const children = new Set(), resources = [];
let interrupted = false;
// Signal only process groups whose leaders we spawned; never global pkill.
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => {
  interrupted = true; for (const child of children) { try { process.kill(-child.pid, 'SIGTERM'); } catch { /* Child already exited. */ } }
});
function command(args, env = {}) {
  return new Promise((resolveResult, reject) => {
    if (interrupted) return reject(new Error('Run interrupted'));
    const child = spawn(process.execPath, [requireUI.resolve('@playwright/test/cli'), ...args], {
      cwd: root, detached: true, env: { ...executionEnv(process.env), ...env }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    children.add(child); let stdout = '', stderr = '';
    child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
    child.once('error', error => { children.delete(child); reject(error); });
    child.once('close', (code, signal) => { children.delete(child); resolveResult({ code, signal, stdout, stderr }); });
  });
}
function printAccount(frontend, account) {
  console.log(`${frontend}: 分母 ${account.denominator}; 应执行数 ${account.expected} vs 实际执行数 ${account.executed}; 显式 skip ${account.explicitSkips}; 待编排 ${account.pendingOrchestration}; ${account.status}`);
  for (const row of account.rows) if (['SKIP', 'NOT_RUN', PENDING_STATUS].includes(row.status)) console.log(`  ${row.status} ${row.file} :: ${row.title} — ${row.reason}${row.source ? ` — ${Object.values(row.source).join(', ')}` : ''}`);
}
function table(inventory, accounts) {
  const cell = value => String(value).replaceAll('|', '\\|').replaceAll('\n', ' ');
  const lines = ['| 用例 | old | new |', '|---|---|---|', ...inventory.map((row, index) => `| ${cell(`${row.file} :: ${row.title}`)} | ${cell(accounts.old?.rows[index]?.status || 'NOT_RUN')} | ${cell(accounts.new?.rows[index]?.status || 'NOT_RUN')} |`)];
  const pending = [];
  for (const frontend of ['old', 'new']) for (const [index, profile] of (accounts[frontend]?.profiles || (accounts[frontend] ? [accounts[frontend]] : [])).entries()) {
    for (const row of profile.rows.filter(row => row.status === PENDING_STATUS)) pending.push(`| ${frontend} / report ${index + 1} | ${cell(`${row.file} :: ${row.title}`)} | ${cell(JSON.stringify(row.rawSkipAnnotations))} | ${cell(row.baselineSkipDescription === null ? '无：基线未写 test.skip 理由（serial 级联）' : row.baselineSkipDescription)} | ${cell(Object.values(row.source).join(', '))} |`);
  }
  if (pending.length) lines.push('', `D 类 ${PENDING_STATUS}：不扣减分母/应执行数，整体仍 FAIL。逐份原件列示（含被其他 profile 执行覆盖的身份）；report 序号对应 result.json profiles 输入顺序。`, '', '| 端 / 原件 | 身份 | 原始 skip annotations | 基线 skip 原文 | 注册 / serial / 编排回源 |', '|---|---|---|---|---|', ...pending);
  return lines.join('\n');
}
function account(inventory, reports, testDir, sides) {
  if (sides.length === 2) return reconcilePair(inventory, reports, testDir, { old: origin('old'), new: origin('new') });
  const frontend = sides[0], result = reconcile(inventory, reports[frontend], testDir), errors = [];
  for (const report of reports[frontend]) if (report.config?.metadata?.frontend !== frontend || report.config?.metadata?.baseURL !== origin(frontend)) errors.push('Invalid report frontend/baseURL provenance');
  return { [frontend]: result, errors, status: errors.length ? 'FAIL' : result.status };
}
const result = { scope: values['self-check'] ? 'offline-self-check' : values['network-self-check'] ? 'network-self-check' : values.reconcile ? 'baseline-reconcile' : `${values.suite}-${frontends.length === 2 ? 'double-run' : 'single-run'}`, output, startedAt: new Date().toISOString(), status: 'FAIL' };
try {
  result.baseline = await verifyBaseline(baseline);
  console.log(`基线 commit: ${result.baseline.head}; clean tracked source`);
  const listed = await command(['test', '--config', resolve(root, 'scripts/fe-parity/inventory.config.mjs'), '--list', '--forbid-only', '--reporter=json']);
  await writeFile(resolve(output, 'inventory.raw.json'), listed.stdout);
  await writeFile(resolve(output, 'inventory.stderr.log'), listed.stderr);
  if (listed.code !== 0) throw new Error(`Baseline --list --forbid-only failed (${listed.code}): ${listed.stderr || listed.stdout}`);
  const inventoryReport = JSON.parse(listed.stdout), historical = JSON.parse(await readFile(historicalInventory, 'utf8'));
  const inventory = reportCases(inventoryReport, resolve(baseline, 'e2e'));
  result.registration = { expected: historical.registered, actual: inventory.length, errors: [...(inventoryReport.errors || []), ...checkInventory(inventory, historical)] };
  console.log(`基线展开注册数: exp007 ${historical.registered} vs 本次 ${inventory.length}; --forbid-only`);
  if (result.registration.errors.length) throw new Error(JSON.stringify(result.registration.errors));
  if (values['self-check'] || values['network-self-check']) {
    result.offline = await selfCheck({ assets: !values['rules-only'], frontends });
    for (const check of result.offline.checks) console.log(`${check.status} ${check.name}${check.error ? `: ${check.error}` : ''}`);
    if (result.offline.status !== 'PASS') throw new Error('Offline self-check failed (see result.json)');
    if (values['network-self-check']) {
      const mock = await startMock(); resources.push(mock);
      for (const frontend of frontends) resources.push(await startFrontend(frontend));
      const { WebSocket } = requireUI('ws');
      for (const frontend of frontends) {
        const response = await fetch(`${origin(frontend)}${frontend === 'new' ? '/probe/leptos/' : '/'}`);
        if (!response.ok || !(await response.text()).includes('<script')) throw new Error(`${frontend}: static entry not served`);
        const login = await fetch(`${origin(frontend)}/api/auth/login`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ username: 'probe', password: 'probe-secret' }) });
        if ((await login.json()).token !== TOKEN) throw new Error(`${frontend}: HTTP proxy failed`);
        await new Promise((done, reject) => {
          const socket = new WebSocket(origin(frontend).replace('http:', 'ws:') + `/ws?token=${TOKEN}`);
          const timer = setTimeout(() => { socket.terminate(); reject(new Error('WS proxy timeout')); }, 5000);
          socket.once('error', error => { clearTimeout(timer); socket.terminate(); reject(error); });
          socket.once('open', () => socket.send(JSON.stringify({ type: 'watch-session', sessionId: 's' })));
          socket.once('message', data => { clearTimeout(timer); socket.close(); if (JSON.parse(data).type !== 'input-queue-state') reject(new Error('Bad WS reply')); else done(); });
        });
      }
      result.network = { status: 'PASS', mock: mock.stats };
      console.log(`PASS loopback static/HTTP/WS ${frontends.join(',')}`);
    }
    // No baseline browser tests are executed by self-check. This is a separate gate.
    result.baselineExecution = { expected: inventory.length, actual: 0, status: 'NOT_RUN', reason: 'self-check only' };
    console.log(`基线应执行数 ${inventory.length} vs 实际执行数 0; NOT_RUN (self-check only)`);
    result.status = 'PASS_SELF_CHECK_ONLY';
  } else {
    const reports = {};
    if (!values.reconcile) {
      result.offline = await selfCheck({ frontends });
      if (result.offline.status !== 'PASS') throw new Error('Preflight failed; run --self-check for details');
      await mkdir(resolve(output, 'browser-tmp'));
      await writeFile(resolve(output, 'mock-runtime.json'), JSON.stringify({ backend: 'f2-mock', projectPath: '/probe' }));
      await writeFile(resolve(output, 'mock-replacement.ndjson'), '');
      const mock = await startMock(); resources.push(mock);
      for (const frontend of frontends) resources.push(await startFrontend(frontend));
      const baselineSuite = values.suite === 'baseline';
      const config = resolve(root, `scripts/fe-parity/${baselineSuite ? 'baseline' : 'browser'}.config.mjs`);
      const testDir = baselineSuite ? resolve(baseline, 'e2e') : resolve(root, 'scripts/fe-parity/tests');
      let registered;
      result.executions = [];
      for (const frontend of frontends) {
        reports[frontend] = [];
        for (const profile of baselineSuite ? profiles : [undefined]) {
          if (interrupted) throw new Error('Run interrupted');
          mock.stream.reset();
          const name = profile ? `${frontend}-${profile}` : frontend;
          const env = executionEnv(process.env, { frontend, output, profile, suite: values.suite });
          // Discover the executing config too: catches profile-dependent registration drift.
          const listing = await command(['test', '--config', config, '--list', '--forbid-only', '--reporter=json'], env);
          await writeFile(resolve(output, `${name}.inventory.json`), listing.stdout);
          await writeFile(resolve(output, `${name}.inventory.stderr.log`), listing.stderr);
          if (listing.code !== 0) throw new Error(`${name} registration failed: ${listing.stderr}`);
          const listReport = JSON.parse(listing.stdout);
          const rows = reportCases(listReport, testDir);
          if (listReport.errors?.length) throw new Error(`${name} registration errors`);
          if (baselineSuite) {
            const errors = checkInventory(rows, historical);
            if (errors.length) throw new Error(JSON.stringify(errors));
          } else {
            const expected = values.suite === 'skeleton' ? 3 : 8;
            if (rows.length !== expected) throw new Error(`${values.suite} registration drift ${rows.length} != ${expected}`);
            if (registered && JSON.stringify(rows.map(({ file, title }) => ({ file, title }))) !== JSON.stringify(registered.map(({ file, title }) => ({ file, title })))) throw new Error('Frontend registration differs');
          }
          registered = rows;
          console.log(`Running ${name}: ${values.suite}, ${rows.length} registered`);
          const execution = await command(['test', '--config', config, '--forbid-only'], env);
          await writeFile(resolve(output, `${name}.stdout.log`), execution.stdout);
          await writeFile(resolve(output, `${name}.stderr.log`), execution.stderr);
          let report;
          try { report = JSON.parse(await readFile(resolve(output, `${name}.json`), 'utf8')); }
          catch { report = { errors: [{ message: `No JSON result: exit ${execution.code}, ${execution.stderr}` }] }; }
          // Preserve the original reporter JSON. Execution errors affect accounting in memory.
          if (execution.code !== 0) (report.errors ||= []).push({ message: `Playwright exit ${execution.code}` });
          reports[frontend].push(report);
          result.executions.push({ frontend, profile, exitCode: execution.code, signal: execution.signal, report: `${name}.json` });
        }
      }
      if (!baselineSuite) {
        result[values.suite] = account(registered, reports, testDir, frontends);
        const comparison = table(registered, result[values.suite]);
        console.log(comparison);
        await writeFile(resolve(output, `${values.suite}.md`), comparison + '\n');
        result.baselineExecution = { expected: inventory.length, actual: 0, status: 'NOT_RUN', reason: `${values.suite} has a separate denominator` };
        result.status = result[values.suite].status === 'PASS' ? (frontends.length === 1 ? 'PASS_SINGLE_FRONTEND_ONLY' : 'PASS_SUITE_ONLY') : 'FAIL';
        // Keep skeleton's historical full-baseline gate, streaming is an explicit standalone F5 gate.
        if (values.suite === 'skeleton') {
          result.accounting = reconcilePair(inventory, { old: { suites: [] }, new: { suites: [] } }, resolve(baseline, 'e2e'), { old: origin('old'), new: origin('new') });
          await writeFile(resolve(output, 'baseline-parity.md'), table(inventory, result.accounting) + '\n');
          result.status = 'FAIL';
        }
      }
      result.mock = mock.stats;

    } else {
      result.reportInputs = { old: values['old-report'] || [], new: values['new-report'] || [] };
      Object.assign(reports, await loadReportPair({ old: values['old-report'], new: values['new-report'] }));
    }
    if (values.reconcile || values.suite === 'baseline') {
      result.accounting = account(inventory, reports, resolve(baseline, 'e2e'), values.reconcile ? ['old', 'new'] : frontends);
      for (const frontend of values.reconcile ? ['old', 'new'] : frontends) printAccount(frontend, result.accounting[frontend]);
      for (const error of result.accounting.errors) console.error(error);
      await writeFile(resolve(output, 'baseline-parity.md'), table(inventory, result.accounting) + '\n');
      result.status = result.accounting.status === 'PASS' && frontends.length === 1 && !values.reconcile ? 'PASS_SINGLE_FRONTEND_ONLY' : result.accounting.status;
    }
  }
} catch (error) { result.error = error.stack; console.error(error.message); }
finally {
  if (!result.accounting && !result.baselineExecution && result.registration) {
    result.baselineExecution = { expected: result.registration.actual, actual: 0, status: 'NOT_RUN', reason: 'Run stopped before browser execution' };
    console.log(`基线应执行数 ${result.registration.actual} vs 实际执行数 0; NOT_RUN (run stopped)`);
  }
  for (const resource of resources.reverse()) try { await resource.close(); } catch (error) { result.status = 'FAIL'; (result.cleanupErrors ||= []).push(error.message); }
  if (interrupted) result.status = 'FAIL';
  await writeFile(resolve(output, 'result.json'), JSON.stringify(result, null, 2) + '\n');
  console.log(`${result.status}: ${resolve(output, 'result.json')}`);
  process.exitCode = result.status.startsWith('PASS') ? 0 : 1;
}
