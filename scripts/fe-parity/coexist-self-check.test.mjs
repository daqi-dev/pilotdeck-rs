// #927 offline self-check for the coexistence suite, the coexist counterpart
// of what self-check.mjs / deploy-self-check.mjs do for their suites:
// spec registration cross-checked against COEXIST_CASES before any browser
// runs, the C3 storage snapshot's actual coverage, the S1/C3 spec wiring, and
// the execution-env guard's rejection cases. Runs with
// `node --test scripts/fe-parity/coexist-self-check.test.mjs`;
// no listen, no browser, no real report files.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { root } from './env.mjs';
import { COEXIST_CASES, coexistTestDir } from './coexist-runner.mjs';
import { assertCoexistExecutionEnv, coexistExecutionEnv, coexistAllowedPorts, coexistPorts, NEW_SHELL_ENTRY_MARKER } from './coexist-deploy.mjs';
import { realPorts } from './env.mjs';
import { ACTIVE_PROJECT_ASSERT, sharedStorageSnapshot } from './coexist-shared-keys.mjs';

const specPath = resolve(coexistTestDir, 'coexist.spec.mjs');
const specSource = () => readFile(specPath, 'utf8');

test('COEXIST_CASES matches the spec registrations phase by phase', async () => {
  const source = await specSource();
  const registered = {};
  for (const match of source.matchAll(/phaseTest\('([a-z]+)', '((?:\\.|[^'])*)'/g)) {
    (registered[match[1]] ||= []).push(match[2]);
  }
  assert.deepEqual(Object.keys(registered).sort(), Object.keys(COEXIST_CASES).sort(),
    'coexist spec 的 phaseTest 阶段必须与 COEXIST_CASES 一致');
  for (const [phase, titles] of Object.entries(COEXIST_CASES)) {
    assert.deepEqual([...(registered[phase] ?? [])].sort(), [...titles].sort(),
      `coexist ${phase} 注册与 COEXIST_CASES 必须逐条一致（离线抓漂移，不等到运行时 registration drift）`);
  }
});

test('the C3 snapshot covers both storages (the active-project key lives in sessionStorage)', () => {
  const fake = values => {
    const store = { ...values };
    Object.defineProperty(store, 'getItem', { value: key => (key in values ? values[key] : null), enumerable: false });
    return store;
  };
  const previous = { local: globalThis.localStorage, session: globalThis.sessionStorage };
  try {
    globalThis.localStorage = fake({ 'auth-token': 'token-1', 'draft_input_general:s': 'draft' });
    globalThis.sessionStorage = fake({ 'pilotdeck-active-project': 'general' });
    const snapshot = sharedStorageSnapshot();
    assert.equal(snapshot['pilotdeck-active-project'], 'general',
      'C3 快照必须覆盖 sessionStorage：活动项目键在 sessionStorage（AppShellV2.tsx:163），只读 localStorage 会漏');
    assert.equal(snapshot['auth-token'], 'token-1', 'C3 快照必须继续覆盖 localStorage 的 auth-token');
    // A cross-shell deletion (or pollution) of the key must break snapshot
    // equality: that is C3's detection power.
    globalThis.sessionStorage = fake({});
    assert.notDeepEqual(sharedStorageSnapshot(), snapshot,
      'C3 检出能力：跨壳删除/污染活动项目键必须使快照不等');
    globalThis.sessionStorage = fake({ 'pilotdeck-active-project': 'polluted' });
    assert.notDeepEqual(sharedStorageSnapshot(), snapshot,
      'C3 检出能力：跨壳污染活动项目键的值必须使快照不等');
  } finally {
    globalThis.localStorage = previous.local;
    globalThis.sessionStorage = previous.session;
  }
});

test('the spec snapshots through the shared helper and asserts the active-project key exists', async () => {
  const source = await specSource();
  assert.ok(source.includes('page.evaluate(sharedStorageSnapshot)'),
    'C3 必须经由共享快照函数采集两类 storage（不得内联只读 localStorage 的闭包）');
  assert.ok(source.includes("sessionStorage.getItem('pilotdeck-active-project')"),
    'C3 必须先断言活动项目键在 sessionStorage 实际存在（评审 P1-2：漏验共享契约）');
  assert.ok(source.includes('ACTIVE_PROJECT_ASSERT'),
    'C3 的存在性断言必须用共享的专属失败消息（ACTIVE_PROJECT_ASSERT）');
});

test('S1 discriminates the new bundle by the probe entrypoint, not the shared boundary boot', async () => {
  const source = await specSource();
  assert.ok(source.includes("').toContain(NEW_SHELL_ENTRY_MARKER)"),
    'S1 必须用新壳专属入口判据（/probe/leptos/probe.js；boundary-boot 双壳共用不能作判据）');
  assert.ok(!source.includes("toContain('/probe/leptos/boundary-boot.js')"),
    'S1 不得退回双壳共用的 boundary-boot 作为新 bundle 判据（评审 P2）');
});

test('the network isolation widens exactly to the coexist ports when FE_SUITE=coexist', async () => {
  // Offline: the runtime monkeypatch itself needs a real run, so assert the
  // two facts that decide it — the port set it would allow, and that
  // real-network.mjs selects that set (and the coexist env guard) for the
  // coexist suite. A source-wiring check, same style as the S1 test above.
  assert.deepEqual([...coexistAllowedPorts].sort((a, b) => a - b),
    [realPorts.mock, coexistPorts.server, coexistPorts.gateway, coexistPorts.vite].sort((a, b) => a - b),
    'coexist 放行端口集必须恰为 mock + 固定共存端口（28420-28422），不多不少');
  const source = await readFile(resolve(root, 'scripts/fe-parity/real-network.mjs'), 'utf8');
  assert.ok(source.includes('const isCoexist = env.FE_SUITE === \'coexist\''),
    'real-network 必须以 FE_SUITE=coexist 识别共存 suite');
  assert.ok(source.includes('if (isCoexist) assertCoexistExecutionEnv(env)'),
    'FE_SUITE=coexist 时必须过共存 env 守卫（fail-closed）');
  assert.ok(source.includes('isCoexist ? coexistAllowedPorts'),
    'FE_SUITE=coexist 时 listen/connect 白名单必须切到 coexistAllowedPorts');
});

test('assertCoexistExecutionEnv rejects tampered provenance', () => {
  // Build one provably good env, then corrupt single fields: each corruption
  // must be rejected fail-closed.
  const dir = resolve(root, '_tmp/fe-parity/self-check-env-probe');
  const good = coexistExecutionEnv({ ...process.env, PILOTDECK_NEW_SHELL: '1' },
    { output: resolve(dir, 'out'), runDir: resolve(dir, 'run'), phase: 'valid', frontend: 'old' });
  const cases = [
    ['rollback 阶段不得携带开关', { ...good, FE_COEXIST_PHASE: 'rollback' }],
    ['valid 阶段开关必须恰为 1', { ...good, PILOTDECK_NEW_SHELL: '0' }],
    ['未知 phase 拒绝', { ...good, FE_COEXIST_PHASE: 'bogus' }],
    ['新侧必须把 REAL_NATIVE_URL 指向共存 origin', { ...good, FE_FRONTEND: 'new', REAL_NATIVE_URL: 'http://127.0.0.1:1/' }],
    ['错端口必须拒绝', { ...good, SERVER_PORT: '28999' }],
    ['非共存 suite 拒绝', { ...good, FE_SUITE: 'real' }],
  ];
  for (const [message, env] of cases) assert.throws(() => assertCoexistExecutionEnv(env), /FAIL|Invalid|must point/, message);
  // The untampered env itself must be accepted (positive control).
  assertCoexistExecutionEnv(good);
});
