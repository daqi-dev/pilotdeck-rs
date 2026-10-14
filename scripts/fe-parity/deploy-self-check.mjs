import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import net from 'node:net';
import { syncBuiltinESMExports } from 'node:module';
import { root, baseline, requireUI } from './env.mjs';
import { executionEnv, prepareRealOSDirectories } from './execution-env.mjs';
import { DEPLOY_CELLS, deployExecutionEnv, assertDeployExecutionEnv, deployCell, deployAllowedPorts, formalProjects, cellInventory, activeRegistration, selectDeployCells, deployRunRoot, deployPorts } from './deploy-matrix.mjs';
import { reportCases, validateDeployRegistration, reconcileDeployCell, reconcileDeploy, SKIP_ALLOWLIST, caseId } from './accounting.mjs';
import { assertDeployPortsFree, assertReplacementReady, failedDeployment } from './deploy-runner.mjs';
import { deploymentConfig, prepareDeployment } from './deploy-launch.mjs';
import { compositionPlugin } from './deploy-vite.mjs';
import { installNetworkIsolation } from './real-network.mjs';
import { tcpRecords, forbiddenTCP, startTCPAudit } from './real-tcp-audit.mjs';
import { staleEntry } from './baseline-stale.mjs';
import { staleControlAttempt } from './baseline-stale-self-check.mjs';
import { proveReplacementBaselineStale } from './replacement-stale-proof.mjs';

// All process/TCP inputs below are injected; --list starts no runtime/browser.
export async function deploySelfCheck(check) {
  await mkdir(resolve(root, '_tmp/fe-parity'), { recursive: true });
  const directory = await mkdtemp(resolve(root, '_tmp/fe-parity/deploy-self-check-'));
  const config = resolve(root, 'scripts/fe-parity/deploy.config.mjs'), testDir = resolve(baseline, 'e2e');
  const reports = {}, envs = {};
  let inventory;
  const list = (config, env) => JSON.parse(execFileSync(process.execPath, [requireUI.resolve('@playwright/test/cli'), 'test', '--config', config,
    '--list', '--forbid-only', '--reporter=json'], { cwd: root, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 30_000 }));
  try {
    await check('B nine cells list unchanged source registrations; nine canonical identities / eighteen required project instances', async () => {
      const frozen = await import(pathToFileURL(resolve(testDir, 'formal-composition.config.mjs')));
      assert.deepEqual(formalProjects, frozen.default.projects, 'Preserve baseline desktop/mobile viewport, touch and mobile semantics');
      inventory = reportCases(list(resolve(root, 'scripts/fe-parity/inventory.config.mjs'), executionEnv(process.env)), testDir);
      assert.equal(inventory.length, 72);
      const b = inventory.filter(row => SKIP_ALLOWLIST.some(item => caseId(item) === caseId(row)));
      assert.equal(b.length, 9);
      const required = [];
      for (const cell of DEPLOY_CELLS) {
        const env = deployExecutionEnv({ ...process.env, REAL_NATIVE_URL: 'http://127.0.0.1:3001', FORMAL_ROUTE_PROFILE: 'wrong',
          REAL_COMPOSITION_URL: 'https://external.invalid', OPENAI_API_KEY: 'operator-secret', NODE_OPTIONS: '--bad' },
        { output: directory, runDir: resolve(directory, cell.id), cell });
        await prepareRealOSDirectories(env);
        envs[cell.id] = env;
        const report = list(config, env);
        assert.deepEqual(report.errors || [], []);
        const rows = reportCases(report, testDir);
        validateDeployRegistration(rows, inventory, cell);
        required.push(...cellInventory(inventory, cell).filter(row => activeRegistration(row, cell)));
        reports[cell.id] = report;
        assert.equal(env.OPENAI_API_KEY, undefined); assert.equal(env.NODE_OPTIONS, undefined);
      }
      assert.equal(required.length, 18);
      assert.equal(new Set(required.map(row => JSON.stringify([row.file, row.canonicalTitle]))).size, 9);
      assert.equal(DEPLOY_CELLS.filter(cell => cell.blocked).length, 2);
      assert.throws(() => selectDeployCells(['native']));
      assert.equal(selectDeployCells(['minimal']).filter(cell => cell.profile === 'native').length, 2);
    });
    await check('B gate env missing/non-owned URL rejects before --list, exact high ports and occupied-port failure', async () => {
      assert(inventory, 'B source listing prerequisite failed');
      for (const cell of DEPLOY_CELLS) {
        const env = envs[cell.id];
        assertDeployExecutionEnv(env);
        for (const name of Object.keys(env).filter(name => /^(FORMAL_|REAL_(?:NATIVE|COMPOSITION))/.test(name))) {
          const missing = { ...env }; delete missing[name];
          assert.throws(() => assertDeployExecutionEnv(missing), /FAIL/);
          assert.throws(() => assertDeployExecutionEnv({ ...env, [name]: 'wrong' }), /FAIL/);
          if (name.endsWith('_URL')) for (const url of ['http://127.0.0.1:3001', 'http://127.0.0.1:18789', 'https://external.invalid', 'http://127.0.0.1:29999', 'http://localhost:28411', 'http://127.0.0.1:28411/'])
            assert.throws(() => assertDeployExecutionEnv({ ...env, [name]: url }), /FAIL/);
        }
        for (const name of ['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME', 'DATABASE_PATH', 'WORKSPACES_ROOT',
          'PILOTDECK_CONFIG_PATH', 'PILOTDECK_FRONTEND_PROFILE', 'SERVER_PORT', 'VITE_PORT', 'PILOTDECK_GATEWAY_PORT', 'HOST', 'SKIP_BROWSER_OPEN', 'PILOTDECK_SKIP_BROWSER_OPEN', 'REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH'])
          assert.throws(() => assertDeployExecutionEnv({ ...env, [name]: '/wrong' }));
      }
      const missing = { ...envs['native-auth'] }; delete missing.REAL_NATIVE_URL;
      assert.throws(() => list(config, missing), error => error.status !== 0 && String(error.stderr).includes('B REAL_NATIVE_URL'));
      const visited = [];
      await assertDeployPortsFree(async port => { visited.push(port); return false; });
      assert.deepEqual(visited, deployAllowedPorts); assert(!visited.includes(3001)); assert(!visited.includes(18789));
      await assert.rejects(assertDeployPortsFree(async port => port === 28411), /occupied/);
      await assert.rejects(assertDeployPortsFree(async () => { throw new Error('EPERM'); }), /EPERM/);
    });
    // Mutate copies of real --list outputs, retaining actual reporter paths,
    // lines, columns, titles and metadata rather than synthetic identities.
    const completed = cell => {
      const report = structuredClone(reports[cell.id]);
      function visit(suite) {
        for (const spec of suite.specs || []) for (const test of spec.tests || []) {
          const active = cell.activeLines.includes(spec.line);
          const binding = SKIP_ALLOWLIST.find(row => row.file === spec.file.split('/').at(-1) && row.title === spec.title)?.skipReasons[1];
          test.annotations = active ? [] : [{ type: 'skip', description: binding.description }];
          test.expectedStatus = active ? 'passed' : 'skipped';
          test.results = [{ status: active ? 'passed' : 'skipped', retry: 0 }];
        }
        for (const child of suite.suites || []) visit(child);
      }
      for (const suite of report.suites) visit(suite);
      return report;
    };
    const firstActive = (report, cell) => {
      const specs = []; const visit = suite => { specs.push(...suite.specs || []); (suite.suites || []).forEach(visit); };
      report.suites.forEach(visit); return specs.find(spec => cell.activeLines.includes(spec.line));
    };
    await check('B replacement stale: frozen host rejects before module call; exact original FAIL stays red and UNPAIRED with zero coverage', async () => {
      assert(inventory, 'B source listing prerequisite failed');
      const proof = await proveReplacementBaselineStale();
      assert.equal(proof.hostUpstreamCalls, 0);
      assert.deepEqual(proof.observations.map(row => row.body.error.code),
        ['MODULE_IDENTITY_UNAVAILABLE', 'COPY_UNAVAILABLE', 'MODULE_IDENTITY_UNAVAILABLE', 'COPY_UNAVAILABLE']);
      for (const cell of DEPLOY_CELLS.filter(cell => cell.profile === 'replacement' && cell.kind !== 'routes')) {
        const make = () => {
          const report = completed(cell);
          const visit = suite => {
            for (const spec of suite.specs || []) if (cell.activeLines.includes(spec.line)) {
              const entry = staleEntry({ file: spec.file.split('/').at(-1), title: spec.title });
              assert(entry, 'Exact B reviewed identity');
              for (const test of spec.tests) test.results = [staleControlAttempt(entry)];
            }
            (suite.suites || []).forEach(visit);
          };
          report.suites.forEach(visit);
          return report;
        };
        const good = reconcileDeployCell(inventory, make(), testDir, cell);
        assert.equal(good.status, 'FAIL'); assert.equal(good.executed, good.expected);
        assert(good.rows.filter(row => row.required).every(row => row.status === 'FAIL' && row.matchedFailCondition
          && row.pairedConsistency === 'UNPAIRED' && row.baselineStale.behavioralCoverageCredit === 0));
        for (const mutate of [
          (r, s) => { s.tests[0].results[0].error.location.line++; },
          (r, s) => { s.tests[0].results[0].error.location.column++; },
          (r, s) => { s.tests[0].results[0].error.message += '\nLocator: borrowed'; },
          (r, s) => { s.tests[0].results[0].error.message = 'Error: wrong failure'; },
          (r, s) => { s.tests[0].results[0].status = 'passed'; },
          (r, s) => { s.tests[0].expectedStatus = 'failed'; },
          (r, s) => { s.tests[0].results[0].status = 'skipped'; },
          (r, s) => { s.tests[0].results[0].retry = 1; },
          r => { r.errors = [{ message: 'unexpected orchestration error' }]; },
          r => { r.config.metadata.cell = 'foreign'; },
        ]) {
          const report = make(); mutate(report, firstActive(report, cell));
          const account = reconcileDeployCell(inventory, report, testDir, cell);
          assert.notEqual(account.rows.find(row => row.required).matchedFailCondition, true);
          // A raw PASS is still a raw PASS, but earns no reviewed failure credit.
          if (firstActive(report, cell).tests[0].results[0].status !== 'passed') assert.equal(account.status, 'FAIL');
        }
      }
    });
    await check('B replacement readiness uses real Node Response/ReadableStream; bad HTTP or implementation identity always FAIL', async () => {
      assert(inventory, 'B source listing prerequisite failed');
      const response = (body, status = 200) => new Response(JSON.stringify(body), {
        status, headers: { 'content-type': 'application/json' },
      });
      const ready = response({ implementationId: 'replacement.knowledge' }), requests = [];
      assert(ready instanceof Response); assert(ready.body instanceof ReadableStream);
      assert.equal(typeof ready.ok, 'boolean'); assert.equal(ready.ok, true);
      await assertReplacementReady(async (url, options) => { requests.push({ url, options }); return ready; });
      assert.equal(ready.bodyUsed, true);
      assert.equal(requests[0].url, `http://127.0.0.1:${deployPorts.knowledge}/healthz`);
      assert(requests[0].options.signal instanceof AbortSignal);
      for (const [makeResponse, expected] of [
        [() => response({ implementationId: 'replacement.knowledge' }, 503), /HTTP 503/],
        [() => response({ implementationId: 'foreign.knowledge' }), /implementationId mismatch/],
        [() => response(null), /implementationId mismatch/],
        [() => new Response('invalid JSON'), /JSON/],
      ]) {
        const health = makeResponse();
        let failure;
        await assert.rejects(async () => {
          try { await assertReplacementReady(async () => health); }
          catch (error) { failure = error; throw error; }
        }, expected);
        assert.equal(health.bodyUsed, true, 'consume or cancel the actual Response body on failure');
        // Mirror the runner catch: readiness failure must stay FAIL/zero
        // execution for all three replacement cells, never become a PASS.
        for (const cell of DEPLOY_CELLS.filter(cell => cell.profile === 'replacement')) {
          const account = reconcileDeployCell(inventory, { errors: [{ message: failure.message }], suites: [] }, testDir, cell);
          assert.equal(account.status, 'FAIL'); assert.equal(account.executed, 0);
          assert.equal(account.expected, cell.kind === 'auth' ? 1 : 2);
          assert(account.errors.includes(failure.message));
        }
      }
      await assert.rejects(assertReplacementReady(async () => { throw new DOMException('injected timeout', 'TimeoutError'); }), /injected timeout/);
    });
    await check('B required env/profile/serial skips always red; exact inactive profile exclusions only', () => {
      assert(inventory, 'B source listing prerequisite failed');
      for (const cell of DEPLOY_CELLS) {
        const original = completed(cell);
        assert.equal(reconcileDeployCell(inventory, original, testDir, cell).status, 'PASS');
        const active = cellInventory(inventory, cell).filter(row => activeRegistration(row, cell));
        for (const registration of active) {
          const catalog = SKIP_ALLOWLIST.find(row => row.file === registration.file && row.title === registration.canonicalTitle);
          for (const reason of [undefined, ...catalog.skipReasons.map(row => row.description)]) {
            const report = completed(cell);
            const find = suite => (suite.specs || []).find(spec => spec.title === registration.title
              && spec.tests.some(test => (test.projectName || '') === registration.project)) || (suite.suites || []).map(find).find(Boolean);
            const spec = report.suites.map(find).find(Boolean), test = spec.tests.find(test => (test.projectName || '') === registration.project);
            test.results[0].status = 'skipped'; test.expectedStatus = 'skipped';
            test.annotations = reason ? [{ type: 'skip', description: reason }] : [];
            const account = reconcileDeployCell(inventory, report, testDir, cell);
            assert.equal(account.status, 'FAIL'); assert.equal(account.expected, active.length);
            assert.equal(account.executed, active.length - 1); assert.equal(account.explicitSkips, 0);
          }
        }
        for (const mutate of [
          r => { firstActive(r, cell).line++; }, r => { firstActive(r, cell).column++; },
          r => { firstActive(r, cell).tests[0].projectName = 'foreign'; },
          r => { firstActive(r, cell).title += 'wrong'; },
          r => { firstActive(r, cell).tests[0].results.push({ status: 'passed', retry: 1 }); },
          r => { firstActive(r, cell).tests[0].results[0].retry = 1; },
          r => { firstActive(r, cell).tests[0].expectedStatus = 'failed'; },
          r => { r.config.metadata.baseURL = 'http://127.0.0.1:3001'; },
          r => { r.config.metadata.cell = 'wrong'; }, r => { r.config.metadata.profile = 'wrong'; },
          r => { r.config.metadata.frontend = 'new'; }, r => { r.config.forbidOnly = false; },
          r => { r.errors = [{ message: 'runtime error' }]; }, r => { r.suites = []; },
          r => { r.suites.push(structuredClone(r.suites[0])); },
        ]) { const report = completed(cell); mutate(report); assert.equal(reconcileDeployCell(inventory, report, testDir, cell).status, 'FAIL'); }
        if (cell.projects.includes('mobile')) {
          const report = completed(cell);
          const removeMobile = suite => {
            for (const spec of suite.specs || []) spec.tests = spec.tests.filter(test => test.projectName !== 'mobile');
            (suite.suites || []).forEach(removeMobile);
          };
          report.suites.forEach(removeMobile);
          const account = reconcileDeployCell(inventory, report, testDir, cell);
          assert.equal(account.status, 'FAIL'); assert.equal(account.expected, active.length);
          assert.equal(account.executed, active.length / 2);
        }
      }
      const runnable = DEPLOY_CELLS.filter(cell => !cell.blocked), inputs = Object.fromEntries(runnable.map(cell => [cell.id, completed(cell)]));
      const partial = reconcileDeploy(inventory, inputs, testDir, runnable);
      assert.equal(partial.status, 'PASS'); assert.equal(partial.executed, 5); assert.equal(partial.instanceExecuted, 11);
      const full = reconcileDeploy(inventory, inputs, testDir, DEPLOY_CELLS);
      assert.equal(full.status, 'FAIL'); assert.equal(full.blocked, 4); assert.equal(full.expected, 9);
      assert.equal(full.executed, 5); assert.equal(full.instanceExpected, 18); assert.equal(full.instanceExecuted, 11);
      const missing = structuredClone(inputs); delete missing['replacement-formal'];
      assert.equal(reconcileDeploy(inventory, missing, testDir, runnable).status, 'FAIL');
      const wrong = structuredClone(inputs); wrong.foreign = wrong['native-auth'];
      assert.equal(reconcileDeploy(inventory, wrong, testDir, runnable).status, 'FAIL');
      for (const selected of [[], runnable.slice(1), [...runnable, runnable[0]]]) assert.equal(reconcileDeploy(inventory, inputs, testDir, selected).status, 'FAIL');
      assert.equal(reconcileDeploy(inventory.filter(row => row.file !== 'real-auth-native.spec.mjs'), inputs, testDir, runnable).status, 'FAIL');
      const drift = inventory.map(row => row.file === 'real-auth-native.spec.mjs' ? { ...row, line: 0 } : row);
      assert.equal(reconcileDeploy(drift, inputs, testDir, runnable).status, 'FAIL');
      const fatal = failedDeployment({ inventory, reports: inputs, testDir, cells: runnable, error: new Error('injected after-cleanup audit failure'), executions: [], runtimeRoot: directory, mock: null });
      assert.equal(fatal.audit.status, 'FAIL'); assert.equal(fatal.accounting.status, 'FAIL');
      assert.equal(fatal.accounting.executed, 5); assert.equal(fatal.accounting.instanceExecuted, 11);
      assert(fatal.accounting.errors.some(error => error.includes('after-cleanup audit failure')));
    });
    await check('B generated profile config and in-memory Vite plugin preserve baseline file and module selection', async () => {
      const generatedPath = resolve(baseline, 'src/composition/generated/frontend-modules.ts');
      const before = await readFile(generatedPath, 'utf8');
      for (const cell of DEPLOY_CELLS.filter(cell => !cell.blocked)) {
        const env = envs[cell.id], profile = await deploymentConfig(cell, env.PILOTDECK_E2E_RUN_DIR);
        assert.equal(profile.webui.runtime.host, '127.0.0.1'); assert.equal(profile.webui.runtime.serverPort, 28410);
        assert.equal(profile.webui.runtime.vitePort, 28411);
        assert.equal(profile.webui.runtime.databasePath, env.DATABASE_PATH);
        assert.equal(profile.model.providers.smoke.url, 'http://127.0.0.1:28379/v1');
        await writeFile(env.PILOTDECK_CONFIG_PATH, JSON.stringify(profile));
        const plugin = await compositionPlugin(env.PILOTDECK_CONFIG_PATH), source = plugin.load(generatedPath);
        assert.equal(plugin.load('/wrong/path'), undefined);
        assert(source.includes('../modules/pilotdeck-chat'));
        assert.equal(source.includes('../modules/pilotdeck-skills'), cell.profile === 'native');
        assert.equal(source.includes('../modules/fixture-knowledge-search'), cell.profile === 'replacement');
        assert(!source.includes('../modules/staffdeck-knowledge'));
        assert(!source.includes('../modules/staffdeck-sop'));
      }
      await assert.rejects(deploymentConfig(deployCell('staffdeck-auth'), directory), /BLOCKED/);
      assert.equal(await readFile(generatedPath, 'utf8'), before);
    });
    await check('B actual fresh runtime seed registers isolated workspace and records B ports; reuse and external paths reject', async () => {
      await mkdir(deployRunRoot, { recursive: true });
      const parent = await mkdtemp(resolve(deployRunRoot, 'offline-seed-'));
      try {
        const cell = deployCell('minimal-auth'), runDir = resolve(parent, 'minimal');
        const env = deployExecutionEnv(process.env, { output: directory, runDir, cell });
        await prepareRealOSDirectories(env);
        const runtime = await prepareDeployment(cell, runDir);
        assert.equal(await readFile(resolve(runtime.registrationPath, '.cwd'), 'utf8'), runtime.projectPath);
        assert.equal(runtime.workspacePath, env.WORKSPACES_ROOT);
        assert(runtime.registrationPath.startsWith(env.PILOT_HOME + '/'));
        const observed = JSON.parse(await readFile(resolve(runDir, 'runtime.json'), 'utf8'));
        assert.deepEqual(observed.ports, deployPorts);
        const profile = JSON.parse(await readFile(env.PILOTDECK_CONFIG_PATH, 'utf8'));
        assert.equal(profile.webui.runtime.databasePath, env.DATABASE_PATH);
        assert.equal(profile.modules.skills.enabled, false);
        await assert.rejects(prepareDeployment(cell, runDir), /EEXIST/);
        await assert.rejects(prepareDeployment(cell, resolve(directory, 'outside-deploy-run')), /canonical real-run\/deploy-run/);
      } finally { await rm(parent, { recursive: true }); }
    });
    await check('B runtime network guard: all allowed sockets stubbed; foreign/public/default ports and wildcard bind blocked', async () => {
      const savedConnect = net.Socket.prototype.connect, savedListen = net.Server.prototype.listen, savedFetch = globalThis.fetch, calls = [];
      try {
        net.Server.prototype.listen = function (...args) { calls.push(args); return this; };
        net.Socket.prototype.connect = function (...args) { calls.push(args); return this; };
        globalThis.fetch = async () => new Response('{}', { headers: { 'content-type': 'application/json' } });
        installNetworkIsolation({ ...envs['native-auth'], FE_NETWORK_LOG: resolve(directory, 'network.jsonl') });
        for (const port of deployAllowedPorts) {
          const server = new net.Server(), socket = new net.Socket();
          assert.equal(server.listen(port, '127.0.0.1'), server);
          assert.equal(socket.connect({ host: '127.0.0.1', port }), socket);
          socket.destroy();
          assert.throws(() => new net.Server().listen(port, '0.0.0.0'), /blocked/);
        }
        for (const port of [3001, 18789, 3010, 5180, 18798, 29999]) {
          assert.throws(() => new net.Server().listen(port, '127.0.0.1'), /blocked/);
          assert.throws(() => new net.Socket().connect({ host: '127.0.0.1', port }), /blocked/);
        }
        assert.throws(() => new net.Socket().connect({ host: 'external.invalid', port: 443 }), /blocked/);
        assert.equal(calls.length, deployAllowedPorts.length * 2);
        const reply = await fetch(`http://127.0.0.1:${deployPorts.server}/api/auth/status`);
        assert(reply instanceof Response); assert(reply.body instanceof ReadableStream);
        assert.equal(reply.ok, true); assert.equal(reply.status, 200);
        assert.deepEqual(await reply.json(), {}); assert.equal(reply.bodyUsed, true);
      } finally {
        net.Server.prototype.listen = savedListen; net.Socket.prototype.connect = savedConnect; globalThis.fetch = savedFetch;
        syncBuiltinESMExports();
      }
    });
    await check('B reuses process/TCP audit; injected safe full run PASS, forbidden TCP fatal before/after cleanup', async () => {
      const raw = endpoint => `p10\nf1\ntIPv4\nn${endpoint}\n`;
      // Accepted server sockets have a peer's ephemeral destination port.
      for (const endpoint of ['127.0.0.1:28410', '127.0.0.1:65000->127.0.0.1:28411', '127.0.0.1:28411->127.0.0.1:65000']) assert.equal(forbiddenTCP(tcpRecords(raw(endpoint)), deployAllowedPorts).length, 0);
      for (const endpoint of ['*:28410', '127.0.0.1:3001', '127.0.0.1:65000->127.0.0.1:18789',
        '127.0.0.1:65000->127.0.0.1:29999', '127.0.0.1:65000->10.0.0.1:443', '[::1]:28410']) assert.equal(forbiddenTCP(tcpRecords(raw(endpoint)), deployAllowedPorts).length, 1);
      const evidence = [], fatal = []; let endpoint = '127.0.0.1:28410';
      const run = async file => file === 'ps' ? '10 1 10 runner' : raw(endpoint);
      const audit = await startTCPAudit({ rootPid: 10, allowedPorts: deployAllowedPorts, run,
        intervalMs: 60_000, record: async row => evidence.push(row), onFatal: error => fatal.push(error.message) });
      try { await audit.checkpoint('B-during'); } finally { await audit.stop(); }
      assert.deepEqual(evidence.map(row => row.phase), ['before-run', 'B-during', 'after-cleanup']);
      assert(evidence.every(row => row.status === 'PASS')); assert.equal(fatal.length, 0);
      endpoint = '127.0.0.1:65000->127.0.0.1:3001';
      await assert.rejects(startTCPAudit({ rootPid: 10, allowedPorts: deployAllowedPorts, run,
        record: async row => evidence.push(row), onFatal: error => fatal.push(error.message) }), /outside B isolated deployment/);
      assert.equal(fatal.length, 1);
      endpoint = '127.0.0.1:28410';
      const cleanup = await startTCPAudit({ rootPid: 10, allowedPorts: deployAllowedPorts, run, intervalMs: 60_000, record: async () => {} });
      endpoint = '127.0.0.1:65000->10.0.0.1:443';
      await assert.rejects(cleanup.stop(), /outside B isolated deployment/);
    });
  } finally { await rm(directory, { recursive: true }); }
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const checks = [];
  await deploySelfCheck(async (name, operation) => {
    try { await operation(); checks.push({ name, status: 'PASS' }); }
    catch (error) { checks.push({ name, status: 'FAIL', error: error.stack }); }
  });
  console.log(JSON.stringify({ scope: 'OFFLINE_ONLY: no listen/Chrome; reporter outcomes and ps/lsof injected', checks }, null, 2));
  process.exitCode = checks.some(check => check.status === 'FAIL') ? 1 : 0;
}
