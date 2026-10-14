import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import { Readable } from 'node:stream';
import { resolve } from 'node:path';
import { createMock, TOKEN } from './mock.mjs';
import { reconcile, checkInventory } from './accounting.mjs';
import { oldDist, newDist, ports, ownedPath } from './env.mjs';
import { assetPath } from './serve.mjs';
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
    const inventory = [{ file: 'a.spec.mjs', title: 'one' }, { file: 'a.spec.mjs', title: 'two' }];
    const make = (status = 'passed', reason = '') => ({ config: { forbidOnly: true }, suites: [{ specs: inventory.map((row, index) => ({ ...row, tests: [{ expectedStatus: 'passed', annotations: reason && index === 1 ? [{ type: 'skip', description: reason }] : [], results: [{ status: index === 1 ? status : 'passed' }] }] })) }] });
    assert.equal(reconcile(inventory, make(), '').status, 'PASS');
    const unguarded = make(); delete unguarded.config; assert.equal(reconcile(inventory, unguarded, '').status, 'FAIL');
    assert.equal(reconcile(inventory, make('skipped', 'profile not applicable'), '').status, 'PASS');
    for (const status of ['skipped', 'interrupted', 'timedOut', 'failed']) assert.equal(reconcile(inventory, make(status), '').status, 'FAIL');
    const missing = make(); missing.suites[0].specs.pop(); assert.equal(reconcile(inventory, missing, '').status, 'FAIL');
    const duplicate = make(); duplicate.suites[0].specs.push(duplicate.suites[0].specs[0]); assert.equal(reconcile(inventory, duplicate, '').status, 'FAIL');
    const retry = make(); retry.suites[0].specs[0].tests[0].results.push({ status: 'passed' }); assert.equal(reconcile(inventory, retry, '').status, 'FAIL');
    const unknown = make(); unknown.suites[0].specs[0].title = 'unknown'; assert.equal(reconcile(inventory, unknown, '').status, 'FAIL');
    const xfail = make(); xfail.suites[0].specs[0].tests[0].expectedStatus = 'failed'; assert.equal(reconcile(inventory, xfail, '').status, 'FAIL');
    assert.equal(checkInventory(inventory, { registered: 2, tests: inventory }).length, 0);
    assert(checkInventory(inventory.slice(1), { registered: 2, tests: inventory }).length > 0);
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
