import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { verifyBaseline } from './baseline.mjs';
import { readFile, readdir, stat, mkdtemp, writeFile, symlink, link, rm, mkdir } from 'node:fs/promises';
import { Readable } from 'node:stream';
import { resolve, dirname } from 'node:path';
import { createMock, TOKEN } from './mock.mjs';
import { reconcile, reconcilePair, loadReportPair, SKIP_ALLOWLIST, caseId, checkInventory } from './accounting.mjs';
import { oldDist, newDist, ports, ownedPath, root, baseline, historicalInventory, origin } from './env.mjs';
import { assetPath } from './serve.mjs';
import { checkMessageMap } from './message-map.mjs';
export async function selfCheck() {
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
  await check('paired reports: 72 PASS vs 72 SKIP, skip identities and provenance', async () => {
    const inventory = JSON.parse(await readFile(historicalInventory, 'utf8')).tests;
    assert.equal(inventory.length, 72);
    assert.equal(SKIP_ALLOWLIST.length, 9);
    assert.equal(checkInventory(inventory, { registered: 72, tests: inventory }).length, 0);
    assert(checkInventory(inventory.slice(1), { registered: 72, tests: inventory }).length > 0);
    const allowed = inventory.flatMap((row, i) => SKIP_ALLOWLIST.some(entry => caseId(entry) === caseId(row)) ? [i] : []);
    assert.equal(allowed.length, 9);
    const origins = { old: origin('old'), new: origin('new') };
    const make = (frontend, skipped = []) => ({ config: { forbidOnly: true, metadata: { frontend, baseURL: origins[frontend] } }, suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed', annotations: skipped.includes(index) ? [{ type: 'skip', description: (SKIP_ALLOWLIST.find(entry => caseId(entry) === caseId(row)) || SKIP_ALLOWLIST[0]).skipReasons[0].description }] : [], results: [{ status: skipped.includes(index) ? 'skipped' : 'passed' }] }] })) }] });
    const pair = (old, next) => reconcilePair(inventory, { old, new: next }, '', origins);
    assert.equal(pair(make('old'), make('new')).status, 'PASS');
    for (const skipped of [allowed.slice(0, 1), allowed]) {
      const accepted = pair(make('old', skipped), make('new', skipped));
      assert.equal(accepted.status, 'PASS');
      for (const side of ['old', 'new']) {
        assert.equal(accepted[side].minimumExecuted, 63);
        assert.equal(accepted[side].executed, 72 - skipped.length);
      }
    }
    const downgrade = pair(make('old'), make('new', inventory.map((_, i) => i)));
    assert.equal(downgrade.status, 'FAIL');
    assert.equal(downgrade.errors.filter(error => error.startsWith('skip identity only in new:')).length, 72);
    assert.equal(pair(make('old', [1]), make('new', [2])).status, 'FAIL');
    assert.equal(pair(make('old', [1]), make('new')).status, 'FAIL');
    assert.equal(pair(make('old'), make('old')).status, 'FAIL');
    assert.equal(pair(make('new'), make('old')).status, 'FAIL');
    for (const frontend of ['old', 'new']) {
      for (const field of ['frontend', 'baseURL']) {
        const reports = { old: make('old'), new: make('new') };
        delete reports[frontend].config.metadata[field];
        assert.equal(reconcilePair(inventory, reports, '', origins).status, 'FAIL');
        reports[frontend].config.metadata[field] = field === 'frontend' ? 'unknown' : 'http://127.0.0.1:28376';
        assert.equal(reconcilePair(inventory, reports, '', origins).status, 'FAIL');
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
        assert.deepEqual(account.errors, []); // Matching skip sets cannot mask per-side failures.
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
    const inventory = [SKIP_ALLOWLIST[5], SKIP_ALLOWLIST[6], SKIP_ALLOWLIST[7]];
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
    try {
      const ui = resolve(fixture, 'ui'); await mkdir(resolve(ui, 'e2e'), { recursive: true });
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'original');
      git(['init', '-q']); git(['add', 'ui']);
      git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'baseline fixture']);
      const head = git(['rev-parse', 'HEAD']);
      await assert.rejects(() => verifyBaseline(ui), /Baseline commit mismatch/);
      assert.equal((await verifyBaseline(ui, head)).status, 'PASS');
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'edited');
      await assert.rejects(() => verifyBaseline(ui, head), /tracked source differs/);
      await writeFile(resolve(ui, 'e2e/example.mjs'), 'original');
      await writeFile(resolve(ui, 'e2e/extra.spec.mjs'), 'untracked');
      await assert.rejects(() => verifyBaseline(ui, head), /untracked e2e/);
      await mkdir(resolve(fixture, 'copy')); await assert.rejects(() => verifyBaseline(resolve(fixture, 'copy'), head), /not an untracked copy/);
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
  await check('baseline full Vite static build exists', async () => {
    const html = await readFile(resolve(oldDist, 'index.html'), 'utf8');
    assert.match(html, /<script[^>]+src=/);
    const assets = await readdir(resolve(oldDist, 'assets')); assert(assets.some(path => path.endsWith('.js')));
    for (const match of html.matchAll(/(?:src|href)="(\/assets\/[^"?#]+)"/g)) assert((await stat(resolve(oldDist, match[1].slice(1)))).size > 0);
    assert.equal(await assetPath('old', '/settings/models'), resolve(oldDist, 'index.html'));
  });
  await check('Leptos JS/Wasm static build and island assets exist', async () => {
    for (const file of ['index.html', 'probe.js', 'probe_bg.wasm']) assert((await stat(resolve(newDist, file))).size > 0);
    assert.deepEqual([...(await readFile(resolve(newDist, 'probe_bg.wasm'))).subarray(0, 4)], [0, 97, 115, 109]);
    assert.equal(await assetPath('new', '/probe/leptos/probe.js'), resolve(newDist, 'probe.js'));
    await assetPath('new', '/shared/island.js'); await assetPath('new', '/vendor/xterm.js');
    await assert.rejects(() => assetPath('new', '/shared/../../CLAUDE.md'));
    await assert.rejects(() => assetPath('old', '/%2e%2e/CLAUDE.md'));
  });
  return { scope: 'OFFLINE_ONLY: handlers and artifacts, no loopback listen or Chrome', status: checks.some(c => c.status === 'FAIL') ? 'FAIL' : 'PASS', checks };
}
