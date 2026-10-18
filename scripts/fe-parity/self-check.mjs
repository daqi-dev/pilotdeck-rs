import { streamSelfCheck } from './stream-self-check.mjs';
import { deploySelfCheck } from './deploy-self-check.mjs';
import { orchestrationSelfCheck } from './orchestration-self-check.mjs';
import { baselineStaleSelfCheck, staleControlAttempt } from './baseline-stale-self-check.mjs';
import { staleEntry } from './baseline-stale.mjs';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { verifyBaseline, lockedImporterVersions, pnpmStoreDir, UNBOUND_PACKAGES } from './baseline.mjs';
import { readFile, readdir, stat, mkdtemp, writeFile, symlink, link, rm, mkdir } from 'node:fs/promises';
import { Readable } from 'node:stream';
import { resolve, dirname } from 'node:path';
import { createMock, TOKEN } from './mock.mjs';
import { reconcile, reconcilePair, loadReportPair, SKIP_ALLOWLIST, caseId, checkInventory } from './accounting.mjs';
import { oldDist, newDist, requireUI, ports, ownedPath, root, baseline, historicalInventory, origin } from './env.mjs';
import { assetPath } from './serve.mjs';
import { checkMessageMap } from './message-map.mjs';
export async function selfCheck({ assets = true, frontends = ['old', 'new'] } = {}) {
  const checks = [];
  async function check(name, run) {
    try { await run(); checks.push({ name, status: 'PASS' }); }
    catch (error) { checks.push({ name, status: 'FAIL', error: error.message }); }
  }
  await check('mock HTTP handlers in process (no listen)', async () => {
    const mock = createMock();
    async function request(path, body, authorization) {
      const req = Readable.from(body === undefined ? [] : [body]);
      Object.assign(req, { url: path, method: body === undefined ? 'GET' : 'POST', headers: { authorization } });
      const reply = { status: 0, writeHead(status) { this.status = status; }, end(data) { this.body = JSON.parse(data); } };
      await mock.handleHTTP(req, reply); return reply;
    }
    assert.equal((await request('/api/auth/status')).body.authDisabled, false);
    assert.equal((await request('/api/auth/login', '{')).status, 400);
    assert.equal((await request('/api/auth/login', JSON.stringify({ username: 'probe', password: 'bad' }))).status, 401);
    assert.equal((await request('/api/auth/login', 'null')).status, 401);
    assert.equal((await request('/api/auth/login', JSON.stringify({ username: 'probe', password: 'probe-secret' }))).body.token, TOKEN);
    assert.equal((await request('/api/projects')).status, 401);
    assert.equal((await request('/api/projects', undefined, `Bearer ${TOKEN}`)).body[0].displayName, 'F2 project');
    assert.equal((await request('/api/projects/probe/sessions', undefined, `Bearer ${TOKEN}`)).body.sessions[0].id, 's');
    assert.equal((await request('/api/missing', undefined, `Bearer ${TOKEN}`)).status, 404);
    const messages = await request('/api/sessions/s/messages', undefined, `Bearer ${TOKEN}`);
    assert.equal(messages.body.messages.length, 48); assert.equal(messages.body.hasMore, false);
    assert.equal((await request('/__parity/stream?action=start&scenario=reading', '')).status, 409);
    const sent = [], socket = { path: '/ws', readyState: 1, send: value => sent.push(JSON.parse(value)) };
    mock.handleMessage(socket, JSON.stringify({ type: 'watch-session', sessionId: 's' }));
    assert.equal((await request('/__parity/stream')).body.watchers, 1);
    try {
      assert.equal((await request('/__parity/stream?action=start&scenario=toString', '')).status, 400);
      assert.equal((await request('/__parity/stream?action=start&scenario=reading', '')).status, 202);
      assert.equal(sent.at(-1).kind, 'status');
      assert.equal((await request('/__parity/stream?action=start&scenario=reading', '')).status, 400);
      mock.handleMessage(socket, JSON.stringify({ type: 'check-session-status', sessionId: 's', statusRequestId: 42 }));
      assert.equal(sent.at(-1).isProcessing, true); assert.equal(sent.at(-1).statusRequestId, 42);
      await request('/__parity/stream?action=stop', ''); assert.equal(mock.stream.state.status, 'cancelled');
      await request('/__parity/stream?action=reset', ''); assert.equal(mock.stream.state.status, 'idle');
      mock.detach(socket); assert.equal((await request('/__parity/stream')).body.watchers, 0);
    } finally { mock.close(); }
  });
  await check('mock WS handlers in process (no listen)', async () => {
    const mock = createMock(), sent = [], socket = { path: '/ws', send: raw => sent.push(JSON.parse(raw)) };
    mock.handleMessage(socket, '{'); mock.handleMessage(socket, 'null');
    mock.handleMessage(socket, JSON.stringify({ type: 'watch-session', sessionId: 's' }));
    assert.equal(sent[0].type, 'input-queue-state'); assert.equal(sent[0].sessionId, 's');
    socket.path = '/shell'; mock.handleMessage(socket, JSON.stringify({ type: 'input', input: '你好' }));
    assert.deepEqual(sent[1], { type: 'output', data: '你好' }); assert.equal(mock.stats.errors.length, 2);
  });
  await check('reconciliation positive and adversarial controls', () => {
    const inventory = [{ file: 'a.spec.mjs', title: 'one' }, SKIP_ALLOWLIST[0]];
    const make = (status = 'passed', reason = '') => ({ config: { forbidOnly: true }, suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed', annotations: reason && index === 1 ? [{ type: 'skip', description: reason }] : [], results: [{ status: index === 1 ? status : 'passed' }] }] })) }] });
    assert.equal(reconcile(inventory, make(), '').status, 'PASS');
    const unguarded = make(); delete unguarded.config; assert.equal(reconcile(inventory, unguarded, '').status, 'FAIL');
    assert.equal(reconcile(inventory, make('skipped', SKIP_ALLOWLIST[0].skipReasons[0].description), '').status, 'PASS');
    for (const field of ['file', 'title']) {
      const changed = inventory.map(row => ({ ...row }));
      changed[1][field] += '-not-allowlisted';
      const report = make('skipped', SKIP_ALLOWLIST[0].skipReasons[0].description);
      report.suites[0].specs[1][field] = changed[1][field];
      const rejected = reconcile(changed, report, '');
      assert.equal(rejected.status, 'FAIL');
      assert.equal(rejected.minimumExecuted, 2);
      assert(rejected.errors.some(error => error.startsWith('skip identity not in fixed allowlist')));
    }
    for (const reason of ['arbitrary reason', ` ${SKIP_ALLOWLIST[0].skipReasons[0].description}`, `${SKIP_ALLOWLIST[0].skipReasons[0].description} `, [SKIP_ALLOWLIST[0].skipReasons[0].description]]) {
      assert.equal(reconcile(inventory, make('skipped', reason), '').status, 'FAIL');
    }
    const multiSkip = make('skipped', SKIP_ALLOWLIST[0].skipReasons[0].description);
    multiSkip.suites[0].specs[1].tests[0].annotations.push({ type: 'skip', description: SKIP_ALLOWLIST[0].skipReasons[0].description });
    assert.equal(reconcile(inventory, multiSkip, '').status, 'FAIL');
    for (const status of ['skipped', 'interrupted', 'timedOut', 'failed']) assert.equal(reconcile(inventory, make(status), '').status, 'FAIL');
    const missing = make(); missing.suites[0].specs.pop(); assert.equal(reconcile(inventory, missing, '').status, 'FAIL');
    const duplicate = make(); duplicate.suites[0].specs.push(duplicate.suites[0].specs[0]); assert.equal(reconcile(inventory, duplicate, '').status, 'FAIL');
    const retry = make(); retry.suites[0].specs[0].tests[0].results.push({ status: 'passed' }); assert.equal(reconcile(inventory, retry, '').status, 'FAIL');
    const unknown = make(); unknown.suites[0].specs[0].title = 'unknown'; assert.equal(reconcile(inventory, unknown, '').status, 'FAIL');
    const xfail = make(); xfail.suites[0].specs[0].tests[0].expectedStatus = 'failed'; assert.equal(reconcile(inventory, xfail, '').status, 'FAIL');
  });
  await check('paired reports: 66 PASS + 5 matched FAIL + 1 human confirmation stays red; 72 SKIP, identities and provenance', async () => {
    const inventory = JSON.parse(await readFile(historicalInventory, 'utf8')).tests;
    assert.equal(inventory.length, 72);
    assert.equal(SKIP_ALLOWLIST.length, 9);
    assert.equal(checkInventory(inventory, { registered: 72, tests: inventory }).length, 0);
    assert(checkInventory(inventory.slice(1), { registered: 72, tests: inventory }).length > 0);
    const allowed = inventory.flatMap((row, i) => SKIP_ALLOWLIST.some(entry => caseId(entry) === caseId(row)) ? [i] : []);
    assert.equal(allowed.length, 9);
    const nonStaleAllowed = allowed.filter(index => !staleEntry(inventory[index]));
    assert.equal(nonStaleAllowed.length, 7);
    const origins = { old: origin('old'), new: origin('new') };
    const testDir = resolve(baseline, 'e2e');
    const make = (frontend, skipped = []) => ({ config: { forbidOnly: true, rootDir: testDir, metadata: { frontend, baseURL: origins[frontend] } }, suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed', annotations: skipped.includes(index) ? [{ type: 'skip', description: (SKIP_ALLOWLIST.find(entry => caseId(entry) === caseId(row)) || SKIP_ALLOWLIST[0]).skipReasons[0].description }] : [], results: [skipped.includes(index) ? { status: 'skipped' } : staleEntry(row) ? staleControlAttempt(staleEntry(row)) : { status: 'passed' }] }] })) }] });
    const pair = (old, next) => reconcilePair(inventory, { old, new: next }, testDir, origins);
    const reviewed = pair(make('old'), make('new'));
    assert.equal(reviewed.status, 'FAIL'); assert.equal(reviewed.needsHumanConfirmation, 1);
    assert.equal(reviewed.baselineStale.filter(row => row.consistency === 'MATCHED_FAIL').length, 5);
    for (const skipped of [nonStaleAllowed.slice(0, 1), nonStaleAllowed]) {
      const accepted = pair(make('old', skipped), make('new', skipped));
      assert.equal(accepted.status, 'FAIL');
      assert.equal(accepted.needsHumanConfirmation, 1);
      assert.equal(accepted.errors.length, 3, 'Only human confirmation plus both formal failure diagnostics remain');
      for (const side of ['old', 'new']) {
        assert.equal(accepted[side].minimumExecuted, 63);
        assert.equal(accepted[side].executed, 72 - skipped.length);
        assert.equal(accepted[side].behavioralCoverage.eligible, 66 - skipped.length);
        assert.equal(accepted[side].behavioralCoverage.passed, 66 - skipped.length);
      }
    }
    assert.equal(pair(make('old', allowed), make('new', allowed)).status, 'FAIL', 'Reviewed B stale cases must execute FAIL, not use env skip');
    const downgrade = pair(make('old'), make('new', inventory.map((_, i) => i)));
    assert.equal(downgrade.status, 'FAIL');
    assert.equal(downgrade.errors.filter(error => error.startsWith('skip identity only in new:')).length, 44); // D's 28 are pending, never SKIP.
    assert.equal(pair(make('old', [1]), make('new', [2])).status, 'FAIL');
    assert.equal(pair(make('old', [1]), make('new')).status, 'FAIL');
    assert.equal(pair(make('old'), make('old')).status, 'FAIL');
    assert.equal(pair(make('new'), make('old')).status, 'FAIL');
    for (const frontend of ['old', 'new']) {
      for (const field of ['frontend', 'baseURL']) {
        const reports = { old: make('old'), new: make('new') };
        delete reports[frontend].config.metadata[field];
        assert.equal(reconcilePair(inventory, reports, testDir, origins).status, 'FAIL');
        reports[frontend].config.metadata[field] = field === 'frontend' ? 'unknown' : 'http://127.0.0.1:28376';
        assert.equal(reconcilePair(inventory, reports, testDir, origins).status, 'FAIL');
      }
    }
  });
  for (const scenario of ['both sides all skipped', 'one side all skipped', 'non-allowlisted skip']) {
    await check(`identity allowlist rejects ${scenario}`, async () => {
      const inventory = JSON.parse(await readFile(historicalInventory, 'utf8')).tests;
      const origins = { old: origin('old'), new: origin('new') };
      const outsider = inventory.find(row => !SKIP_ALLOWLIST.some(entry => caseId(entry) === caseId(row)));
      const reports = Object.fromEntries(['old', 'new'].map(frontend => [frontend, {
        config: { forbidOnly: true, metadata: { frontend, baseURL: origins[frontend] } },
        suites: [{ specs: inventory.map(row => {
          const skip = scenario === 'both sides all skipped' || (scenario === 'one side all skipped' ? frontend === 'new' : caseId(row) === caseId(outsider));
          return { ...row, tests: [{ expectedStatus: 'passed', annotations: skip ? [{ type: 'skip', description: SKIP_ALLOWLIST[0].skipReasons[0].description }] : [], results: [{ status: skip ? 'skipped' : 'passed' }] }] };
        }) }],
      }]));
      const account = reconcilePair(inventory, reports, '', origins);
      assert.equal(account.status, 'FAIL');
      assert.equal(account.new.status, 'FAIL');
      assert(account.new.errors.some(error => error.startsWith('skip identity not in fixed allowlist')));
      if (scenario === 'non-allowlisted skip') {
        // Even above the 63-case floor, one unauthorized skip is a failure.
        assert.equal(account.new.executed, 71);
        assert.equal(account.new.explicitSkips, 0);
      } else {
        assert.equal(account.new.executed, 0);
        assert.equal(account.new.minimumExecuted, 63);
        assert(account.new.errors.some(error => error.includes('最低执行数 63')));
      }
      if (scenario !== 'one side all skipped') {
        assert.equal(account.old.status, 'FAIL');
        assert(!account.errors.some(error => error.startsWith('skip identity only')));
        assert(account.errors.some(error => error.startsWith('old: skip identity not in fixed allowlist'))); // Matching sets cannot mask per-side failures.
      }
    });
  }
  await check('every source-bound skip reason accepts original and rejects one-character drift', async () => {
    for (const registration of SKIP_ALLOWLIST) {
      for (const binding of registration.skipReasons) {
        const [path, line] = binding.source.split(':');
        const source = (await readFile(resolve(dirname(baseline), path), 'utf8')).split('\n')[Number(line) - 1];
        assert(source.includes('test.skip('));
        assert(source.includes(`'${binding.description}'`)); // All nine identities use literal reasons at this commit.
        const make = (row, reason) => ({ config: { forbidOnly: true }, suites: [{ specs: [{ ...row,
          tests: [{ expectedStatus: 'skipped', annotations: [{ type: 'skip', description: reason }], results: [{ status: 'skipped' }] }],
        }] }] });
        assert.equal(reconcile([registration], make(registration, binding.description), '').status, 'PASS');
        assert.equal(reconcile([registration], make(registration, binding.description.slice(0, -1) + '!'), '').status, 'FAIL');
        const outsider = { ...registration, title: registration.title + ' outsider' };
        assert.equal(reconcile([outsider], make(outsider, binding.description), '').status, 'FAIL');
        const foreign = SKIP_ALLOWLIST.flatMap(row => row.skipReasons).find(reason => !registration.skipReasons.some(own => own.description === reason.description));
        assert.equal(reconcile([registration], make(registration, foreign.description), '').status, 'FAIL');
      }
    }
  });
  await check('profile merge counts execution once and never masks invalid original reports', () => {
    const inventory = SKIP_ALLOWLIST.filter(row => !staleEntry(row)).slice(0, 3);
    const origins = { old: origin('old'), new: origin('new') };
    const make = (frontend, active) => ({ config: { forbidOnly: true, metadata: { frontend, baseURL: origins[frontend] } },
      suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed',
        annotations: index === active ? [] : [{ type: 'skip', description: row.skipReasons[1].description }],
        results: [{ status: index === active ? 'passed' : 'skipped' }],
      }] })) }] });
    const reports = () => Object.fromEntries(['old', 'new'].map(frontend => [frontend, [0, 1, 2].map(active => make(frontend, active))]));
    const pair = input => reconcilePair(inventory, input, '', origins);
    const accepted = pair(reports());
    assert.equal(accepted.status, 'PASS');
    assert.equal(accepted.needsHumanConfirmation, 0);
    assert.equal(accepted.old.executed, 3); assert.equal(accepted.old.explicitSkips, 0);
    const repeated = reports(); repeated.old.push(make('old', 0));
    assert.equal(pair(repeated).old.executed, 3);
    assert.equal(pair(repeated).status, 'PASS');
    for (const mutate of [
      input => { input.old[0].suites[0].specs[0].tests[0].results[0].status = 'failed'; },
      input => { input.old[0].suites[0].specs[1].tests[0].annotations[0].description += '!'; },
      input => { input.old[0].suites[0].specs.pop(); },
      input => { input.old[0].suites[0].specs.push(input.old[0].suites[0].specs[0]); },
      input => { input.old[0].suites[0].specs[0].tests[0].results.push({ status: 'passed' }); },
      input => { input.old[0].config.metadata.frontend = 'new'; },
      input => { input.old[0].config.forbidOnly = false; },
      input => { input.old[0].errors = [{ message: 'run failed' }]; },
      input => { input.old = []; },
      input => { input.old.pop(); }, // Merged skip sets must still agree.
    ]) { const input = reports(); mutate(input); assert.equal(pair(input).status, 'FAIL'); }
  });
  await check('baseline commit gate rejects different HEAD, edited checkout and untracked copy', async () => {
    assert.equal((await verifyBaseline(baseline)).status, 'PASS');
    const fixture = await mkdtemp(resolve(root, '_tmp/fe-parity/baseline-control-'));
    const git = args => execFileSync('git', ['-C', fixture, ...args], { encoding: 'utf8', env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' } }).trim();
    // This fixture has no pnpm install; node_modules is covered by the next check.
    const GIT_ONLY = { checkNodeModules: false };
    try {
      const ui = resolve(fixture, 'ui'); await mkdir(resolve(ui, 'e2e'), { recursive: true });
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'original');
      git(['init', '-q']); git(['add', 'ui']);
      git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'baseline fixture']);
      const head = git(['rev-parse', 'HEAD']);
      await assert.rejects(() => verifyBaseline(ui, undefined, GIT_ONLY), /Baseline commit mismatch/);
      assert.equal((await verifyBaseline(ui, head, GIT_ONLY)).status, 'PASS');
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'edited');
      await assert.rejects(() => verifyBaseline(ui, head, GIT_ONLY), /tracked source differs/);
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'original');
      await writeFile(resolve(ui, 'e2e/extra.spec.mjs'), 'untracked');
      await assert.rejects(() => verifyBaseline(ui, head, GIT_ONLY), /untracked e2e/);
      await mkdir(resolve(fixture, 'copy')); await assert.rejects(() => verifyBaseline(resolve(fixture, 'copy'), head, GIT_ONLY), /not an untracked copy/);
    } finally { await rm(fixture, { recursive: true }); }
  });
  await check('baseline node_modules gate binds every package the runner requireUI()s', async () => {
    // Every target of a requireUI call or requireUI.resolve call, in any scripts/fe-parity
    // file, must be a direct dependency the gate binds (ui or root manifest).
    const used = new Set();
    const walk = async dir => {
      for (const entry of await readdir(dir, { withFileTypes: true })) {
        const path = resolve(dir, entry.name);
        if (entry.isDirectory()) { await walk(path); continue; }
        if (!/\.(mjs|js)$/.test(entry.name)) continue;
        for (const m of (await readFile(path, 'utf8')).matchAll(/requireUI(?:\.resolve)?\(\s*['"]([^'"]+)['"]/g)) {
          const [first, second] = m[1].split('/');
          used.add(first.startsWith('@') ? `${first}/${second}` : first);
        }
      }
    };
    await walk(resolve(root, 'scripts/fe-parity'));
    const declaredBy = async dir => {
      const m = JSON.parse(await readFile(resolve(dir, 'package.json'), 'utf8'));
      return Object.keys({ ...m.dependencies, ...m.devDependencies });
    };
    const bound = new Set([...await declaredBy(baseline), ...await declaredBy(dirname(baseline)), ...UNBOUND_PACKAGES]);
    assert.ok(used.has('vite') && used.has('typescript'), 'scanner found the known call sites');
    assert.deepEqual([...used].filter(name => !bound.has(name)).sort(), []);
  });
  await check('baseline node_modules gate rejects missing/diverged lockfile and links outside .pnpm', async () => {
    assert.equal((await verifyBaseline(baseline)).status, 'PASS');
    const fixture = await mkdtemp(resolve(root, '_tmp/fe-parity/baseline-nm-control-'));
    const git = args => execFileSync('git', ['-C', fixture, ...args], { encoding: 'utf8', env: { ...process.env, GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1' } }).trim();
    try {
      const ui = resolve(fixture, 'ui'); await mkdir(resolve(ui, 'e2e'), { recursive: true });
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'original');
      const store = resolve(fixture, 'node_modules', '.pnpm');
      const pkg = async (dir, name) => {
        const dirPath = resolve(store, dir, 'node_modules', name);
        await mkdir(dirPath, { recursive: true });
        return dirPath;
      };
      for (const [dir, name, version] of [
        ['vite@7.3.3_aaa@1.0.0', 'vite', '7.3.3'],
        ['ws@8.18.0', 'ws', '8.18.0'],
        ['ws@8.21.0', 'ws', '8.21.0'],
        ['@playwright+test@1.60.0', '@playwright/test', '1.60.0'],
        ['@xterm+xterm@5.5.0', '@xterm/xterm', '5.5.0'],
        ['react@18.3.1', 'react', '18.3.1'],
        ['react-dom@18.3.1_react@18.3.1', 'react-dom', '18.3.1'],
        ['typescript@5.9.3', 'typescript', '5.9.3'],
        ['yaml@2.9.0', 'yaml', '2.9.0'],
        ['yaml@2.8.0', 'yaml', '2.9.0'], // directory name right, package.json version wrong below
        ['jsonrepair@3.14.0', 'jsonrepair', '3.14.0'],
      ]) {
        const dirPath = await pkg(dir, name);
        await writeFile(resolve(dirPath, 'package.json'), JSON.stringify({ name, version }));
      }
      // Same shape as the real baseline: ui/node_modules/<name> -> <repo>/node_modules/.pnpm/<dir>/node_modules/<name>.
      const link = async (name, dir) => {
        await mkdir(dirname(resolve(ui, 'node_modules', name)), { recursive: true });
        await symlink(resolve(store, dir, 'node_modules', name), resolve(ui, 'node_modules', name));
      };
      await link('vite', 'vite@7.3.3_aaa@1.0.0');
      await link('ws', 'ws@8.21.0');
      await link('@playwright/test', '@playwright+test@1.60.0');
      await link('@xterm/xterm', '@xterm+xterm@5.5.0');
      for (const [name, dir] of [['react', 'react@18.3.1'], ['react-dom', 'react-dom@18.3.1_react@18.3.1'],
        ['typescript', 'typescript@5.9.3'], ['yaml', 'yaml@2.9.0']]) await link(name, dir);
      // Root importer packages resolve from <repo>/node_modules (requireUI walks up from ui).
      await symlink(resolve(store, 'jsonrepair@3.14.0', 'node_modules', 'jsonrepair'), resolve(fixture, 'node_modules', 'jsonrepair'));
      // Manifest and lockfile pin the wanted set; versions bind the links (#61.2).
      const uiManifest = { dependencies: { ws: '^8', '@xterm/xterm': '^5', react: '^18', 'react-dom': '^18', yaml: '^2' },
        devDependencies: { vite: '^7', '@playwright/test': '^1', typescript: '^5' } };
      await writeFile(resolve(ui, 'package.json'), JSON.stringify(uiManifest));
      await writeFile(resolve(fixture, 'package.json'), JSON.stringify({ dependencies: { jsonrepair: '^3' } }));
      const entry = (name, version) => `      ${name.startsWith('@') ? `'${name}'` : name}:\n        specifier: x\n        version: ${version}\n`;
      const lockOf = ({ dropYaml = false } = {}) => 'lockfileVersion: 9\n\nimporters:\n\n  .:\n    dependencies:\n' + entry('jsonrepair', '3.14.0')
        + '\n  ui:\n    dependencies:\n' + entry('@xterm/xterm', '5.5.0') + entry('react', '18.3.1') + entry('react-dom', '18.3.1(react@18.3.1)')
        + entry('ws', '8.21.0') + (dropYaml ? '' : entry('yaml', '2.9.0'))
        + '    devDependencies:\n' + entry('@playwright/test', '1.60.0') + entry('typescript', '5.9.3') + entry('vite', '7.3.3(aaa@1.0.0)') + '\npackages:\n';
      const lock = lockOf();
      await writeFile(resolve(fixture, 'pnpm-lock.yaml'), lock);
      const modulesYaml = max => writeFile(resolve(fixture, 'node_modules', '.modules.yaml'),
        JSON.stringify({ nodeLinker: 'isolated', virtualStoreDir: '.pnpm', virtualStoreDirMaxLength: max }));
      await mkdir(resolve(fixture, 'node_modules'), { recursive: true }); await modulesYaml(120);
      // Like the real baseline, installed modules are untracked.
      await writeFile(resolve(fixture, '.gitignore'), 'node_modules/\n');
      git(['init', '-q']); git(['add', 'ui', '.gitignore', 'pnpm-lock.yaml', 'package.json']);
      git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'baseline nm fixture']);
      const head = git(['rev-parse', 'HEAD']);
      // Missing node_modules/.pnpm/lock.yaml
      await assert.rejects(() => verifyBaseline(ui, head), /node_modules lockfile missing/);
      // Diverged node_modules/.pnpm/lock.yaml
      await writeFile(resolve(store, 'lock.yaml'), 'lockfileVersion: 8\n');
      await assert.rejects(() => verifyBaseline(ui, head), /lockfile differs/);
      await writeFile(resolve(store, 'lock.yaml'), lock);
      // Package link points outside .pnpm
      const stray = resolve(fixture, 'stray-ws'); await mkdir(resolve(stray, 'node_modules', 'ws'), { recursive: true });
      await writeFile(resolve(stray, 'node_modules', 'ws', 'package.json'), JSON.stringify({ name: 'ws', version: '8.21.0' }));
      await rm(resolve(ui, 'node_modules', 'ws'));
      await (await import('node:fs/promises')).symlink(resolve(stray, 'node_modules', 'ws'), resolve(ui, 'node_modules', 'ws'));
      await assert.rejects(() => verifyBaseline(ui, head), /resolves outside \.pnpm/);
      await rm(resolve(ui, 'node_modules', 'ws')); await link('ws', 'ws@8.21.0');
      // The well-formed fixture passes and reports every package, @xterm included.
      const good = await verifyBaseline(ui, head);
      assert.deepEqual(good.nodeModules.packages.map(p => `${p.importer}:${p.name}`),
        ['ui:@playwright/test', 'ui:@xterm/xterm', 'ui:react', 'ui:react-dom', 'ui:typescript', 'ui:vite', 'ui:ws', 'ui:yaml', '.:jsonrepair']);
      // pnpm's store names, checked against real baseline directories: peers nest,
      // and long names are truncated plus a SHA-256 prefix.
      assert.equal(pnpmStoreDir('react-router-dom', '6.30.3(react-dom@18.3.1(react@18.3.1))(react@18.3.1)'),
        'react-router-dom@6.30.3_react-dom@18.3.1_react@18.3.1__react@18.3.1');
      assert.equal(pnpmStoreDir('@replit/codemirror-minimap', '0.5.2(@codemirror/language@6.12.3)(@codemirror/lint@6.9.6)(@codemirror/state@6.6.0)(@codemirror/view@6.43.0)(@lezer/common@1.5.2)(@lezer/highlight@1.2.3)'),
        '@replit+codemirror-minimap@0.5.2_@codemirror+language@6.12.3_@codemirror+lint@6.9.6_@co_f948bbb4c9ec9485279b50606a015b3c');
      // Store directory name right but the package inside reports another version.
      await writeFile(resolve(store, 'yaml@2.8.0', 'node_modules', 'yaml', 'package.json'), JSON.stringify({ name: 'yaml', version: '2.9.0' }));
      await writeFile(resolve(store, 'yaml@2.9.0', 'node_modules', 'yaml', 'package.json'), JSON.stringify({ name: 'yaml', version: '2.8.0' }));
      await assert.rejects(() => verifyBaseline(ui, head), /yaml version mismatch: 2\.8\.0 != 2\.9\.0/);
      await writeFile(resolve(store, 'yaml@2.9.0', 'node_modules', 'yaml', 'package.json'), JSON.stringify({ name: 'yaml', version: '2.9.0' }));
      // Unsupported layouts and malformed limits are refused.
      await writeFile(resolve(fixture, 'node_modules', '.modules.yaml'), JSON.stringify({ nodeLinker: 'hoisted', virtualStoreDir: '.pnpm', virtualStoreDirMaxLength: 120 }));
      await assert.rejects(() => verifyBaseline(ui, head), /layout unsupported/);
      await writeFile(resolve(fixture, 'node_modules', '.modules.yaml'), JSON.stringify({ nodeLinker: 'isolated', virtualStoreDir: '.pnpm', virtualStoreDirMaxLength: '120' }));
      await assert.rejects(() => verifyBaseline(ui, head), /virtualStoreDirMaxLength unsupported: 120/);
      await modulesYaml(120);
      // Lockfile reader fails closed on a missing name, a duplicate, or a missing importer.
      assert.throws(() => lockedImporterVersions(lockOf({ dropYaml: true }), 'ui', ['yaml']), /importers\.ui does not resolve yaml/);
      assert.throws(() => lockedImporterVersions(lock.replace(entry('ws', '8.21.0'), entry('ws', '8.21.0') + entry('ws', '8.21.0')), 'ui', ['ws']), /lists ws twice/);
      assert.throws(() => lockedImporterVersions(lock, 'apps/desktop', []), /no importers\.apps\/desktop section/);
      // A layout whose store names would be hashed is refused explicitly.
      await modulesYaml(10);
      await assert.rejects(() => verifyBaseline(ui, head), /virtualStoreDirMaxLength unsupported: 10/);
      await modulesYaml(120);
      // Lockfile unchanged, link moved to another valid version in the store.
      await rm(resolve(ui, 'node_modules', 'ws')); await link('ws', 'ws@8.18.0');
      await assert.rejects(() => verifyBaseline(ui, head), /ws resolves to ws@8\.18\.0, lockfile expects ws@8\.21\.0/);
      await rm(resolve(ui, 'node_modules', 'ws')); await link('ws', 'ws@8.21.0');
      // A declared @xterm link deleted, or the whole scope gone, fails.
      await rm(resolve(ui, 'node_modules', '@xterm', 'xterm'));
      await assert.rejects(() => verifyBaseline(ui, head), /missing package: @xterm\/xterm/);
      await rm(resolve(ui, 'node_modules', '@xterm'), { recursive: true });
      await assert.rejects(() => verifyBaseline(ui, head), /missing package: @xterm\/xterm/);
      await link('@xterm/xterm', '@xterm+xterm@5.5.0');
      // An installed @xterm package the manifest does not declare fails.
      await pkg('@xterm+extra@1.0.0', '@xterm/extra'); await link('@xterm/extra', '@xterm+extra@1.0.0');
      await assert.rejects(() => verifyBaseline(ui, head), /undeclared @xterm packages: @xterm\/extra/);
      await rm(resolve(ui, 'node_modules', '@xterm', 'extra'));
      // An @xterm link outside the store is rejected, not skipped.
      const strayXterm = resolve(fixture, 'stray-xterm', 'node_modules', '@xterm', 'xterm');
      await mkdir(strayXterm, { recursive: true });
      await writeFile(resolve(strayXterm, 'package.json'), JSON.stringify({ name: '@xterm/xterm', version: '5.5.0' }));
      await rm(resolve(ui, 'node_modules', '@xterm', 'xterm'));
      await symlink(strayXterm, resolve(ui, 'node_modules', '@xterm', 'xterm'));
      await assert.rejects(() => verifyBaseline(ui, head), /@xterm\/xterm resolves outside \.pnpm/);
    } finally { await rm(fixture, { recursive: true }); }
  });
  await check('report files must differ: same path, symlink and hardlink rejected', async () => {
    const directory = resolve(root, '_tmp/fe-parity'); await mkdir(directory, { recursive: true });
    const fixture = await mkdtemp(resolve(directory, 'report-file-control-'));
    try {
      const old = resolve(fixture, 'old.json'), next = resolve(fixture, 'new.json');
      await writeFile(old, '{"suites":[]}'); await writeFile(next, '{"suites":[]}');
      await loadReportPair({ old, new: next });
      await assert.rejects(() => loadReportPair({ old, new: old }), /different files/);
      await assert.rejects(() => loadReportPair({ old: [old, old], new: [next] }), /different files/);
      assert.equal((await loadReportPair({ old: [old], new: [next] })).old.length, 1);
      await symlink(old, resolve(fixture, 'symlink.json'));
      await link(old, resolve(fixture, 'hardlink.json'));
      for (const alias of ['symlink.json', 'hardlink.json']) await assert.rejects(() => loadReportPair({ old, new: resolve(fixture, alias) }), /different files/);
    } finally { await rm(fixture, { recursive: true }); }
  });
  await check('WS message map: every table link matches its identifier or explicit branch', async () => {
    const markdown = await readFile(resolve(root, 'docs/research/007_frontend/ws-message-map.md'), 'utf8');
    const checked = await checkMessageMap(markdown, dirname(baseline));
    assert.deepEqual(checked.errors, []);
    assert(checked.links > 100);
    // Original stream_delta regression must fail even though line 864 exists.
    const drifted = markdown.replaceAll('useChatRealtimeHandlers.ts:847', 'useChatRealtimeHandlers.ts:864').replaceAll('useChatRealtimeHandlers.ts#L847', 'useChatRealtimeHandlers.ts#L864');
    assert((await checkMessageMap(drifted, dirname(baseline))).errors.some(error => error.includes('stream_delta')));
    const unmarked = markdown.replace('（分支起点，无字面量；代码：`default:`；字面量：', '（无标注：');
    assert((await checkMessageMap(unmarked, dirname(baseline))).errors.some(error => error.includes('active-sessions')));
  });
  await check('worktree output and port constraints', () => {
    assert.deepEqual(Object.values(ports), [28376, 28377, 28378]);
    assert.throws(() => ownedPath('/tmp/external')); assert.throws(() => ownedPath(resolve(oldDist, '../../outside')));
  });
  await streamSelfCheck(check);
  await orchestrationSelfCheck(check);
  await deploySelfCheck(check);
  await baselineStaleSelfCheck(check);
  await check('mock LLM + real orchestration unit tests (no listen/Chrome)', () => {
    execFileSync(process.execPath, ['--test', resolve(root, 'scripts/fe-parity/mock-llm.test.mjs')], { cwd: root, encoding: 'utf8' });
  });
  await check('real ready/onboarding configs register the exact disjoint D 28', async () => {
    const { realExecutionEnv, prepareRealOSDirectories } = await import('./execution-env.mjs');
    const { validateRealRegistration } = await import('./real-runner.mjs');
    const { reportCases, PENDING_ORCHESTRATION } = await import('./accounting.mjs');
    const directory = await mkdtemp(resolve(root, '_tmp/fe-parity/real-list-'));
    try {
      for (const mode of ['ready', 'onboarding']) {
        const env = realExecutionEnv(process.env, { output: directory, runDir: resolve(directory, mode), mode });
        await prepareRealOSDirectories(env);
        const raw = execFileSync(process.execPath, [requireUI.resolve('@playwright/test/cli'), 'test', '--config', resolve(root, 'scripts/fe-parity/real.config.mjs'), '--list', '--forbid-only', '--reporter=json'], { cwd: root, env, encoding: 'utf8' });
        const report = JSON.parse(raw);
        assert.deepEqual(report.errors || [], []);
        validateRealRegistration(reportCases(report, resolve(baseline, 'e2e')), PENDING_ORCHESTRATION, mode);
      }
    } finally { await rm(directory, { recursive: true }); }
  });
  if (assets && frontends.includes('old')) await check('baseline full Vite static build exists', async () => {
    const html = await readFile(resolve(oldDist, 'index.html'), 'utf8');
    assert.match(html, /<script[^>]+src=/);
    for (const file of ['composition.html', 'e2e/fixtures/model-selection.html', 'e2e/fixtures/streaming-lifecycle.html', 'e2e/fixtures/streaming-scroll.html']) {
      const fixture = await readFile(resolve(oldDist, file), 'utf8');
      assert.match(fixture, /<script[^>]+src=/);
      assert.equal(await assetPath('old', `/${file}`), resolve(oldDist, file));
    }
    const assets = await readdir(resolve(oldDist, 'assets')); assert(assets.some(path => path.endsWith('.js')));
    for (const match of html.matchAll(/(?:src|href)="(\/assets\/[^"?#]+)"/g)) assert((await stat(resolve(oldDist, match[1].slice(1)))).size > 0);
    assert.equal(await assetPath('old', '/settings/models'), resolve(oldDist, 'index.html'));
  });
  if (assets && frontends.includes('new')) await check('Leptos JS/Wasm static build and island assets exist', async () => {
    for (const file of ['index.html', 'probe.js', 'probe_bg.wasm']) assert((await stat(resolve(newDist, file))).size > 0);
    assert.deepEqual([...(await readFile(resolve(newDist, 'probe_bg.wasm'))).subarray(0, 4)], [0, 97, 115, 109]);
    assert.equal(await assetPath('new', '/probe/leptos/probe.js'), resolve(newDist, 'probe.js'));
    await assetPath('new', '/shared/island.js'); await assetPath('new', '/vendor/xterm.js');
    await assert.rejects(() => assetPath('new', '/shared/../../CLAUDE.md'));
    await assert.rejects(() => assetPath('old', '/%2e%2e/CLAUDE.md'));
  });
  return { scope: 'OFFLINE_ONLY: no loopback listen or Chrome', artifactsChecked: assets ? frontends : [], status: checks.some(c => c.status === 'FAIL') ? 'FAIL' : 'PASS', checks };
}
