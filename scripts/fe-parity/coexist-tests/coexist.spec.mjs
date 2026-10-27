// #927 F3 coexistence probe spec: bidirectional navigation, shared keys and the
// rollback switch against the real native TS deployment (start-built, Option A
// patch, boundary installed in both shells). Runs phase-driven via
// FE_COEXIST_PHASE; each phase is its own deployment run.
import { writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { ownedPath, requireUI } from '../env.mjs';
import { NEW_SHELL_ENTRY_MARKER } from '../coexist-deploy.mjs';
import { ACTIVE_PROJECT_ASSERT, ACTIVE_PROJECT_KEY, sharedStorageSnapshot } from '../coexist-shared-keys.mjs';
const { test, expect } = requireUI('@playwright/test');

const phase = process.env.FE_COEXIST_PHASE;
const output = ownedPath(process.env.FE_OUTPUT);
const PROBE_HEADING = 'F1 Leptos probe';
const OLD_DRAFT = 'coexist-draft-old-927';
const NEW_DRAFT = 'coexist-draft-new-927';

// Playwright requires the callback's first argument to be a destructuring
// pattern, so the wrapper names the fixtures the suite uses and forwards them.
function phaseTest(expected, title, fn) {
  test(title, async ({ page, request }) => {
    test.skip(phase !== expected, `coexist phase is ${phase}, not ${expected}`);
    await fn({ page, request });
  });
}
async function evidence(id, data) {
  await writeFile(resolve(output, `coexist-${phase}-${id}.json`), JSON.stringify({ phase, id, ...data }, null, 2) + '\n');
}
// Same real account flow as the D suite's global setup (Create Account, or Sign
// in when the phase's fresh database already has the account).
async function signInOldShell(page) {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  const create = page.getByRole('button', { name: 'Create Account' });
  const login = page.getByRole('button', { name: /Sign in|登录|Log in/i });
  await expect(create.or(login).first()).toBeVisible({ timeout: 30_000 });
  const username = 'e2e-user', password = 'e2e-password-123';
  if (await create.isVisible()) {
    await page.getByLabel('Username').fill(username);
    await page.getByLabel('Password', { exact: true }).fill(password);
    await page.getByLabel('Confirm Password').fill(password); await create.click();
  } else {
    await page.getByLabel(/Username|用户名/i).fill(username);
    await page.getByLabel(/Password|密码/i).fill(password); await login.click();
  }
  await expect(create).toBeHidden({ timeout: 30_000 });
  await expect(login).toBeHidden({ timeout: 30_000 });
  await expect.poll(() => page.evaluate(() => localStorage.getItem('auth-token')), { timeout: 30_000 }).toBeTruthy();
}
const settingsButton = page => page.getByRole('button', { name: /^(Settings|设置)$/ });
async function oldShellReady(page) {
  await expect(settingsButton(page)).toBeVisible({ timeout: 30_000 });
}
async function newShellReady(page) {
  await expect(page.getByRole('heading', { name: PROBE_HEADING })).toBeVisible();
}
const setMarker = page => page.evaluate(() => { window.__coexistMarker = 'coexist-927'; });
const marker = page => page.evaluate(() => window.__coexistMarker);
const navType = page => page.evaluate(() => performance.getEntriesByType('navigation')?.[0]?.type);
const draftKeys = page => page.evaluate(() =>
  Object.fromEntries(Object.entries(localStorage).filter(([key]) => key.startsWith('draft_input_'))));

phaseTest('valid', 'S1 开关开：GET /settings 新 bundle、GET /settingsX 旧 bundle（HTTP 严格段边界）', async ({ request }) => {
  const migrated = await request.get('/settings');
  expect(migrated.status()).toBe(200);
  const migratedBody = await migrated.text();
  expect(migratedBody, '/settings 必须回新 shell 入口（新壳专属 probe 入口模块）').toContain(NEW_SHELL_ENTRY_MARKER);
  const sibling = await request.get('/settingsX');
  expect(sibling.status()).toBe(200);
  const siblingBody = await sibling.text();
  expect(siblingBody, '严格段边界：/settingsX 必须保持旧 bundle').toContain('id="root"');
  // boundary-boot.js 双壳共用不能作判据；探针入口模块只被新壳 HTML 引用。
  expect(siblingBody, '严格段边界：/settingsX 不得引用探针入口模块').not.toContain(NEW_SHELL_ENTRY_MARKER);
  await evidence('S1', { migratedStatus: migrated.status(), siblingStatus: sibling.status() });
});

phaseTest('valid', 'N1 旧→新 navigate()：sidebar Settings 硬跳新 bundle', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  await setMarker(page);
  await settingsButton(page).click();
  await expect(page).toHaveURL('/settings');
  await newShellReady(page);
  expect(await marker(page), '硬跳 = 整页加载，marker 必须消失').toBeUndefined();
  await evidence('N1', { url: page.url(), markerGone: true, entry: 'AppShellV2.tsx:387 navigate(SETTINGS_PATH)' });
});

// Records every history mutation (kind, url, current path) across same-origin
// documents, so the evidence shows which shell's navigation changed the stack.
async function recordHistoryMutations(page) {
  await page.addInitScript(() => {
    const log = (kind, url) => {
      try {
        const entries = JSON.parse(sessionStorage.getItem('__coexistHist') || '[]');
        entries.push({ kind, url: String(url), at: location.pathname });
        sessionStorage.setItem('__coexistHist', JSON.stringify(entries));
      } catch { /* storage unavailable */ }
    };
    for (const [name, kind] of [['pushState', 'push'], ['replaceState', 'replace']]) {
      const original = history[name].bind(history);
      history[name] = (...args) => { log(kind, args[2]); return original(...args); };
    }
  });
}
// Chrome's own session-history view (definitive entry list + current index).
async function chromeHistory(page) {
  const cdp = await page.context().newCDPSession(page);
  try {
    const history = await cdp.send('Page.getNavigationHistory');
    return { index: history.currentIndex,
      entries: history.entries.map(entry => entry.url.replace(/^https?:\/\/[^/]+/, '')) };
  } finally { await cdp.detach(); }
}

// N3 is the ORIGINAL acceptance cell from issue #927: a traversal departing
// from the old shell (forward, after crossing to the new shell and coming
// back) must land the new bundle at /settings. Measured today it does NOT
// (the frozen old shell's reload boot navigation prunes the forward entry,
// characterized in N3b): the case therefore records an honest FAIL against
// the original criterion instead of being re-scored as a characterization
// pass. The remediation options are a human decision (moved to issue #937:
// change no-store / drop the boot push / accept no forward).
phaseTest('valid', 'N3 前进/后退跨壳遍历（原验收格）：旧壳侧出发 goForward 进入 /settings 必须落新 bundle', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  const from = page.url(); // 登录后旧壳停在项目路由（如 /p/general）
  await settingsButton(page).click();
  await newShellReady(page);
  await page.goBack();
  await expect(page).toHaveURL(from);
  await oldShellReady(page);
  const fwd = await page.goForward();
  await expect(page).toHaveURL('/settings');
  await newShellReady(page);
  await evidence('N3', { from, url: page.url(), goForwardResponse: fwd ? fwd.status() : null,
    note: '原验收格照原判据执行（评审 P1-1）：旧壳侧出发的前进必须落新 bundle。' +
      '当前实测被冻结旧壳固有行为阻断（整页重载后启动 pushState 清跨壳前进项，根因特征化见 N3b，决策已转 issue #937 待定夺）。' +
      'BFCache 归因如实标注：本装置 Chrome 由 Playwright 以 --disable-back-forward-cache 启动，BFCache 整体关闭；' +
      '「server no-store 使旧壳不命中 BFCache」为读码推断（HTTP 标准语义），未在本装置实测。' +
      'D4 对账：跨壳后退为跨文档导航，落在正确壳，状态由 URL+storage 重建；跨壳视觉闪烁仅人眼（待人工验收）。' });
});

// N3b characterizes the blocked cell (root-cause evidence for N3's honest
// FAIL): after the old shell fully reloads (which every back into it does in
// this harness — Playwright launches Chrome with --disable-back-forward-cache,
// so BFCache is off regardless of cache headers; whether the server's no-store
// would independently keep a real-user browser out of BFCache is 读码推断,
// standard HTTP semantics, not measured here), its boot navigation runs
// handleProjectSelect → navigate('/') — a same-document pushState
// (AppShellV2.tsx:207-223 syncs the URL project back afterwards) — which
// replaces the cross-shell forward entry, so goForward has nothing to
// traverse into the new shell. This is frozen old-shell behaviour, not a
// defect of the A patch or the boundary; the test pins it so that any change
// (old-shell fix, cache header change, BFCache) forces a re-characterization
// instead of silently pretending forward works.
phaseTest('valid', 'N3b 前进入新壳（特征化）：旧壳重载后其启动导航清除跨壳前进项', async ({ page }) => {
  await recordHistoryMutations(page);
  await signInOldShell(page);
  await oldShellReady(page);
  const from = page.url();
  await settingsButton(page).click();
  await newShellReady(page);
  const appended = await chromeHistory(page);
  expect(appended.entries[appended.index], '硬跳后前进项必须是 /settings').toBe('/settings');
  await page.goBack();
  await expect(page).toHaveURL(from);
  await oldShellReady(page);
  // The reloaded old shell prunes the forward entry once its projects load and
  // the URL-sync effect runs; poll Chrome's own history view for the prune.
  await expect.poll(() => chromeHistory(page).then(history => history.entries.includes('/settings')),
    { timeout: 30_000 }).toBe(false);
  const afterPrune = await chromeHistory(page);
  const mutations = await page.evaluate(() => JSON.parse(sessionStorage.getItem('__coexistHist') || '[]'));
  const bootPush = mutations.find(entry => entry.kind === 'push' && entry.url === '/' && entry.at === '/p/general');
  expect(bootPush, '根因必须可观测：旧壳启动导航 pushState(\'/\')（AppShellV2.tsx:207-223 handleProjectSelect → navigate(\'/\')）').toBeTruthy();
  const fwd = await page.goForward();
  await page.waitForTimeout(1000);
  expect(page.url(), '前进项已被清除，goForward 不得再导航（特征化断言，待 F3 定夺）').toContain('/p/general');
  await oldShellReady(page);
  await evidence('N3b', { appended, afterPrune, mutations, goForwardResponse: fwd ? fwd.status() : null,
    note: '前进入新壳被冻结旧壳固有行为阻断：① 后退入旧壳在本装置必整页重载——本装置 Chrome 由 Playwright 以 --disable-back-forward-cache 启动，BFCache 整体关闭（tcp-connections.jsonl 进程 argv 实测）；server no-store 是否独立阻断真实用户浏览器的 BFCache 为读码推断（HTTP 标准语义），未实测；② 重载启动时 URL 同步 effect 调 handleProjectSelect → navigate(\'/\') pushState，清掉跨壳前进项。②为冻结 TS 侧行为，非本包 A patch/boundary 缺陷；修复需 F3 定夺（改 no-store / 去启动 push / 接受不可前进入新壳，决策已转 issue #937）。' });
});

phaseTest('valid', 'N4 新→旧 「返回旧壳」链接硬跳', async ({ page }) => {
  await signInOldShell(page);
  await page.goto('/settings');
  await newShellReady(page);
  await setMarker(page);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  // The link hard-jumps to '/', but the old shell's default-project effect may
  // client-redirect '/' to the project route right after load (the same boot
  // navigation N3b characterizes); either landing is the old bundle in control.
  await expect(page).toHaveURL(/\/$|\/p\/general$/);
  await oldShellReady(page);
  expect(await marker(page), '硬跳 = 整页加载，marker 必须消失').toBeUndefined();
  await evidence('N4', { url: page.url(), markerGone: true, entry: 'probe-leptos main.rs <a href="/">',
    note: '链接目标为 /；旧壳启动可能随后把 / 客户端重定向到 /p/general（冻结旧壳固有行为，见 N3b）' });
});

phaseTest('valid', 'N5 新→旧 history 原语被拦截硬跳', async ({ page }) => {
  await signInOldShell(page);
  await page.goto('/settings');
  await newShellReady(page);
  await setMarker(page);
  await page.evaluate(() => { history.pushState({}, '', '/'); });
  // The boundary hard-jumps to '/', the old shell's boot may then client-redirect
  // to the project route (same frozen-shell behaviour as N4/N3b).
  await expect(page).toHaveURL(/\/$|\/p\/general$/);
  await oldShellReady(page);
  expect(await marker(page), 'boundary 拦截 pushState 后必须整页硬跳').toBeUndefined();
  await evidence('N5', { url: page.url(), markerGone: true, entry: 'boundary.js wrapped history.pushState',
    note: '拦截目标为 /；旧壳启动可能随后把 / 客户端重定向到 /p/general（见 N3b）' });
});

phaseTest('valid', 'C1 登录态互通（auth-token 跨壳）', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  const token = await page.evaluate(() => localStorage.getItem('auth-token'));
  const authorizations = [];
  page.on('request', entry => {
    if (entry.url().includes('/api/projects/probe/sessions')) authorizations.push(entry.headers().authorization);
  });
  await page.goto('/settings');
  await newShellReady(page);
  await expect(page.locator('#composer'), '探针必须以登录态挂载（读同一 auth-token）').toBeVisible();
  await expect.poll(() => authorizations.length, { timeout: 30_000 }).toBeGreaterThan(0);
  expect(authorizations[0], '探针 API 请求的 Authorization 必须等于旧壳 token').toBe(`Bearer ${token}`);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  await oldShellReady(page);
  await expect(page.getByRole('button', { name: /Create Account/ })).toBeHidden();
  await evidence('C1', { probeRequests: authorizations.length, tokenPrefix: token.slice(0, 8), reloginRequired: false });
});

phaseTest('valid', 'C2a 旧壳 composer 草稿跨壳保留', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  const composer = page.getByPlaceholder(/Tell PilotDeck|告诉 PilotDeck/);
  await expect(composer).toBeVisible();
  await composer.fill(OLD_DRAFT);
  await expect.poll(() => page.evaluate(text =>
    Object.keys(localStorage).filter(key => key.startsWith('draft_input_') && localStorage.getItem(key) === text).length, OLD_DRAFT),
  { timeout: 30_000 }).toBeGreaterThan(0);
  const before = await draftKeys(page);
  await settingsButton(page).click();
  await newShellReady(page);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  await oldShellReady(page);
  await expect(composer, '旧壳草稿跨壳往返必须恢复（丢即 bug）').toHaveValue(OLD_DRAFT);
  expect(await draftKeys(page), 'draft_input_* 键值跨壳往返必须不变').toEqual(before);
  await evidence('C2a', { draftKeys: Object.keys(before), restored: true });
});

phaseTest('valid', 'C2b 探针 composer 草稿跨壳保留', async ({ page }) => {
  await signInOldShell(page);
  await page.goto('/settings');
  await newShellReady(page);
  const composer = page.locator('#composer');
  await expect(composer).toBeVisible();
  await composer.fill(NEW_DRAFT);
  await expect.poll(() => page.evaluate(() => localStorage.getItem('draft_input_probe:s')),
  { timeout: 30_000 }).toBe(NEW_DRAFT);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  await oldShellReady(page);
  await settingsButton(page).click();
  await newShellReady(page);
  await expect(composer, '探针草稿跨壳往返必须恢复（丢即 bug）').toHaveValue(NEW_DRAFT);
  await evidence('C2b', { draftKey: 'draft_input_probe:s', restored: true });
});

phaseTest('valid', 'C3 共享键跨跳转不变', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  // The active-project key must actually exist before any equality claim:
  // it lives in sessionStorage (AppShellV2.tsx:163), invisible to a
  // localStorage-only snapshot — the gap this case existed to hide.
  await expect.poll(() => page.evaluate(() => sessionStorage.getItem('pilotdeck-active-project')),
    { message: ACTIVE_PROJECT_ASSERT, timeout: 30_000 }).toBeTruthy();
  // The snapshot is namespaced per storage (fix-r2 P0-3): a key moved across
  // storages or shadowed by a same-named key in the other storage must break
  // equality, never pass through a flat merge of the two.
  const before = await page.evaluate(sharedStorageSnapshot);
  expect(before.localStorage['auth-token'], 'C1 前置：登录后 auth-token 必须存在（localStorage 归属）').toBeTruthy();
  await settingsButton(page).click();
  await newShellReady(page);
  expect(await page.evaluate(sharedStorageSnapshot), '跳到新壳后共享键（localStorage+sessionStorage，按 storage 归属）必须不变').toEqual(before);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  await oldShellReady(page);
  expect(await page.evaluate(sharedStorageSnapshot), '跳回旧壳后共享键（localStorage+sessionStorage，按 storage 归属）必须不变').toEqual(before);
  await evidence('C3', { localKeys: Object.keys(before.localStorage), sessionKeys: Object.keys(before.sessionStorage),
    activeProject: before.sessionStorage[ACTIVE_PROJECT_KEY] ?? null,
    storages: ['localStorage', 'sessionStorage'] });
});

phaseTest('valid', 'D1 /ws 跨壳往返重连（D1 对账）', async ({ page }) => {
  const sockets = [];
  page.on('websocket', socket => {
    const entry = { url: socket.url(), frames: 0 };
    sockets.push(entry);
    socket.on('framereceived', () => { entry.frames += 1; });
  });
  await signInOldShell(page);
  await oldShellReady(page);
  await expect.poll(() => sockets.some(entry => entry.url.includes('/ws')), { timeout: 30_000 }).toBeTruthy();
  const before = sockets.map(entry => ({ ...entry }));
  await settingsButton(page).click();
  await newShellReady(page);
  await page.getByRole('link', { name: '返回旧壳' }).click();
  await oldShellReady(page);
  await expect.poll(() => sockets.filter(entry => entry.url.includes('/ws')).length,
  { timeout: 30_000 }).toBeGreaterThan(before.filter(entry => entry.url.includes('/ws')).length);
  await page.waitForTimeout(3000); // 帧活动为观察项（3s 采样），重连本身是断言
  await evidence('D1', { sockets, before,
    note: 'D1 对账：/ws 重连为断言；帧计数为观察。/shell（D2）为探针终端连接，mount 重建即新连接；同文档退避梯阶 exp007 已测；D3 未测（待 F5 聊天面）' });
});

phaseTest('invalid', 'N2 旧→新 <Link>：ModelConfigurationErrorScreen 硬跳新 bundle', async ({ page }) => {
  await signInOldShell(page);
  const link = page.getByRole('link', { name: /^(Open model settings|打开模型设置)$/ });
  await expect(link, 'invalid 配置必须渲染 ModelConfigurationErrorScreen').toBeVisible({ timeout: 30_000 });
  await setMarker(page);
  await link.click();
  await expect(page).toHaveURL('/settings/models');
  await newShellReady(page);
  expect(await marker(page), '硬跳 = 整页加载，marker 必须消失').toBeUndefined();
  await evidence('N2', { url: page.url(), markerGone: true, entry: 'ModelConfigurationErrorScreen.tsx:40 <Link to="/settings/models">' });
});

phaseTest('rollback', 'R1 rollback：GET /settings 旧 bundle 照常服务', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  await page.goto('/settings');
  await expect(page.locator('.pilotdeck-settings-app'), '开关关后旧 bundle 必须照常服务 /settings').toBeVisible();
  await expect(page.getByRole('heading', { name: PROBE_HEADING })).toBeHidden();
  await evidence('R1', { url: page.url(), servedBy: 'old bundle (PILOTDECK_NEW_SHELL unset)' });
});

phaseTest('rollback', 'R2 rollback：旧壳 Settings 导航落在旧 bundle', async ({ page }) => {
  await signInOldShell(page);
  await oldShellReady(page);
  await setMarker(page);
  await settingsButton(page).click();
  await expect(page).toHaveURL('/settings');
  await expect(page.locator('.pilotdeck-settings-app')).toBeVisible();
  expect(await marker(page), '已知回退形态：boundary 硬跳后由旧 bundle 接住（多一次整页加载）').toBeUndefined();
  await evidence('R2', { url: page.url(), note: '回退形态记录：client boundary 仍硬跳，旧 bundle 在 /settings 整页加载后渲染设置页' });
});
