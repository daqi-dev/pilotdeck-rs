#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { baseline, root, requireUI, historicalInventory, ownedPath, origin } from './env.mjs';
import { reportCases, checkInventory, reconcile } from './accounting.mjs';
import { selfCheck } from './self-check.mjs';
import { startMock, TOKEN } from './mock.mjs';
import { startFrontend } from './serve.mjs';
const { values } = parseArgs({ options: {
  help: { type: 'boolean' }, 'self-check': { type: 'boolean' }, 'network-self-check': { type: 'boolean' },
  reconcile: { type: 'boolean' }, suite: { type: 'string', default: 'skeleton' },
  frontends: { type: 'string', default: 'old,new' }, 'old-report': { type: 'string' }, 'new-report': { type: 'string' },
  output: { type: 'string' },
} });
if (values.help) {
  console.log(`Usage (from worktree root; no browser downloads):
  node scripts/fe-parity/run.mjs --self-check
  node scripts/fe-parity/run.mjs --network-self-check    # coordinator only: loopback HTTP/WS
  node scripts/fe-parity/run.mjs --suite skeleton --frontends old,new
  node scripts/fe-parity/run.mjs --reconcile --old-report <Playwright JSON> --new-report <Playwright JSON>

Always discovers baseline ui/e2e with --list --forbid-only, compares exp007's 72 registrations.
Skeleton = 3 new auth/transport cases against one mock, actual old Vite / Leptos probe.
Skeleton cannot cover the 72 baseline cases: full reconciliation remains FAIL (exit 1).
--reconcile also fails without both full reports; skipped tests require explicit skip descriptions.
--self-check does not listen or launch Chrome, fails if static assets are absent.
All generated reports/profiles/screenshots live under this worktree _tmp/fe-parity.
Ports: mock 28376 / old 28377 / new 28378, bind only 127.0.0.1.
See scripts/fe-parity/README.md for source builds and next PR scope.`);
  process.exit(0);
}
if (values.frontends !== 'old,new' || values.suite !== 'skeleton') throw new Error('This PR supports only --suite skeleton --frontends old,new; deployment suites are next PR');
if ([values['self-check'], values['network-self-check'], values.reconcile].filter(Boolean).length > 1) throw new Error('Select one run mode');
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
      cwd: root, detached: true, env: { ...process.env, ...env, PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: '1' }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    children.add(child); let stdout = '', stderr = '';
    child.stdout.on('data', data => { stdout += data; }); child.stderr.on('data', data => { stderr += data; });
    child.once('error', error => { children.delete(child); reject(error); });
    child.once('close', (code, signal) => { children.delete(child); resolveResult({ code, signal, stdout, stderr }); });
  });
}
function printAccount(frontend, account) {
  console.log(`${frontend}: 分母 ${account.denominator}; 应执行数 ${account.expected} vs 实际执行数 ${account.executed}; 显式 skip ${account.explicitSkips}; ${account.status}`);
  for (const row of account.rows) if (['SKIP', 'NOT_RUN'].includes(row.status)) console.log(`  ${row.status} ${row.file} :: ${row.title} — ${row.reason}`);
}
function table(inventory, accounts) {
  const cell = value => String(value).replaceAll('|', '\\|').replaceAll('\n', ' ');
  return ['| 用例 | old | new |', '|---|---|---|', ...inventory.map((row, index) => `| ${cell(`${row.file} :: ${row.title}`)} | ${cell(accounts.old.rows[index].status)} | ${cell(accounts.new.rows[index].status)} |`)].join('\n');
}
const result = { scope: values['self-check'] ? 'offline-self-check' : values['network-self-check'] ? 'network-self-check' : values.reconcile ? 'baseline-reconcile' : 'skeleton-double-run', output, startedAt: new Date().toISOString(), status: 'FAIL' };
try {
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
    result.offline = await selfCheck();
    for (const check of result.offline.checks) console.log(`${check.status} ${check.name}${check.error ? `: ${check.error}` : ''}`);
    if (result.offline.status !== 'PASS') throw new Error('Offline self-check failed (see result.json)');
    if (values['network-self-check']) {
      const mock = await startMock(); resources.push(mock);
      for (const frontend of ['old', 'new']) resources.push(await startFrontend(frontend));
      const { WebSocket } = requireUI('ws');
      for (const frontend of ['old', 'new']) {
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
      console.log('PASS loopback static/HTTP/WS both frontends');
    }
    // No baseline browser tests are executed by self-check. This is a separate gate.
    result.baselineExecution = { expected: inventory.length, actual: 0, status: 'NOT_RUN', reason: 'self-check only' };
    console.log(`基线应执行数 ${inventory.length} vs 实际执行数 0; NOT_RUN (self-check only)`);
    result.status = 'PASS_SELF_CHECK_ONLY';
  } else {
    const reports = {};
    if (!values.reconcile) {
      result.offline = await selfCheck();
      if (result.offline.status !== 'PASS') throw new Error('Preflight failed; run --self-check for details');
      resources.push(await startMock());
      for (const frontend of ['old', 'new']) resources.push(await startFrontend(frontend));
      for (const frontend of ['old', 'new']) {
        const execution = await command(['test', '--config', resolve(root, 'scripts/fe-parity/browser.config.mjs'), '--forbid-only'], { FE_FRONTEND: frontend, FE_OUTPUT: output });
        await writeFile(resolve(output, `${frontend}.stdout.log`), execution.stdout);
        await writeFile(resolve(output, `${frontend}.stderr.log`), execution.stderr);
        try { reports[frontend] = JSON.parse(await readFile(resolve(output, `${frontend}.json`), 'utf8')); }
        catch { reports[frontend] = { errors: [{ message: `No JSON result: exit ${execution.code}, ${execution.stderr}` }] }; }
        if (execution.code !== 0) (reports[frontend].errors ||= []).push({ message: `Playwright exit ${execution.code}` });
      }
      const skeletonInventory = reportCases(reports.old, resolve(root, 'scripts/fe-parity/tests')).map(({ file, title }) => ({ file, title }));
      // Independent discovery prevents both frontends omitting the same test and going green.
      const skeletonList = await command(['test', '--config', resolve(root, 'scripts/fe-parity/browser.config.mjs'), '--list', '--forbid-only', '--reporter=json'], { FE_FRONTEND: 'old', FE_OUTPUT: output });
      if (skeletonList.code !== 0) throw new Error('Skeleton registration failed');
      const registered = reportCases(JSON.parse(skeletonList.stdout), resolve(root, 'scripts/fe-parity/tests'));
      result.skeleton = { old: reconcile(registered, reports.old, resolve(root, 'scripts/fe-parity/tests')), new: reconcile(registered, reports.new, resolve(root, 'scripts/fe-parity/tests')) };
      if (registered.length !== 3) throw new Error(`Skeleton registration drift: ${registered.length} != 3`);
      const skeletonTable = table(registered, result.skeleton);
      console.log(`骨架逐例对照（独立于基线72）\n${skeletonTable}`);
      await writeFile(resolve(output, 'skeleton.md'), skeletonTable + '\n');
      // These new tests are not baseline registrations; deliberately do not credit them.
      reports.old = { suites: [] }; reports.new = { suites: [] };
      result.skeleton.reported = skeletonInventory.length;
    } else {
      for (const frontend of ['old', 'new']) reports[frontend] = values[`${frontend}-report`]
        ? JSON.parse(await readFile(resolve(values[`${frontend}-report`]), 'utf8')) : { suites: [] };
    }
    result.accounting = {};
    for (const frontend of ['old', 'new']) { result.accounting[frontend] = reconcile(inventory, reports[frontend], resolve(baseline, 'e2e')); printAccount(frontend, result.accounting[frontend]); }
    await writeFile(resolve(output, 'baseline-parity.md'), table(inventory, result.accounting) + '\n');
    result.status = Object.values(result.accounting).every(account => account.status === 'PASS') ? 'PASS' : 'FAIL';
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
