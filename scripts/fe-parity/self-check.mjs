import assert from 'node:assert/strict';
import { readFile, readdir, stat, mkdtemp, writeFile, symlink, link, rm, mkdir } from 'node:fs/promises';
import { Readable } from 'node:stream';
import { resolve, dirname } from 'node:path';
import { createMock, TOKEN } from './mock.mjs';
import { reconcile, reconcilePair, loadReportPair, SKIP_REASONS, SKIP_ALLOWLIST, caseId, checkInventory } from './accounting.mjs';
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
    assert.equal(reconcile(inventory, make('skipped', SKIP_REASONS[0]), '').status, 'PASS');
    for (const field of ['file', 'title']) {
      const changed = inventory.map(row => ({ ...row }));
      changed[1][field] += '-not-allowlisted';
      const report = make('skipped', SKIP_REASONS[0]);
      report.suites[0].specs[1][field] = changed[1][field];
      const rejected = reconcile(changed, report, '');
      assert.equal(rejected.status, 'FAIL');
      assert.equal(rejected.minimumExecuted, 2);
      assert(rejected.errors.some(error => error.startsWith('skip identity not in fixed allowlist')));
    }
    for (const reason of ['arbitrary reason', ` ${SKIP_REASONS[0]}`, `${SKIP_REASONS[0]} `, [SKIP_REASONS[0]]]) {
      assert.equal(reconcile(inventory, make('skipped', reason), '').status, 'FAIL');
    }
    const multiSkip = make('skipped', SKIP_REASONS[0]);
    multiSkip.suites[0].specs[1].tests[0].annotations.push({ type: 'skip', description: SKIP_REASONS[0] });
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
    const make = (frontend, skipped = []) => ({ config: { forbidOnly: true, metadata: { frontend, baseURL: origins[frontend] } }, suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed', annotations: skipped.includes(index) ? [{ type: 'skip', description: SKIP_REASONS[0] }] : [], results: [{ status: skipped.includes(index) ? 'skipped' : 'passed' }] }] })) }] });
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
          return { ...row, tests: [{ expectedStatus: 'passed', annotations: skip ? [{ type: 'skip', description: SKIP_REASONS[0] }] : [], results: [{ status: skip ? 'skipped' : 'passed' }] }] };
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
  await check('report files must differ: same path, symlink and hardlink rejected', async () => {
    const directory = resolve(root, '_tmp/fe-parity'); await mkdir(directory, { recursive: true });
    const fixture = await mkdtemp(resolve(directory, 'report-file-control-'));
    try {
      const old = resolve(fixture, 'old.json'), next = resolve(fixture, 'new.json');
      await writeFile(old, '{"suites":[]}'); await writeFile(next, '{"suites":[]}');
      await loadReportPair({ old, new: next });
      await assert.rejects(() => loadReportPair({ old, new: old }), /different files/);
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
