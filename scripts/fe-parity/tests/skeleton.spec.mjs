import { mkdir, mkdtemp } from 'node:fs/promises';
import { resolve } from 'node:path';
import { root, requireUI, origin, ownedPath } from '../env.mjs';
import { TOKEN } from '../mock.mjs';
const { test: base, expect, chromium } = requireUI('@playwright/test');
const frontend = process.env.FE_FRONTEND;
const output = ownedPath(process.env.FE_OUTPUT);
// Override context so Playwright never launches its bundled browser. Profiles are per test.
const test = base.extend({
  context: async ({}, use) => {
    const directory = resolve(root, '_tmp/fe-parity/profiles');
    await mkdir(directory, { recursive: true });
    const profile = await mkdtemp(resolve(directory, `${frontend}-`));
    const context = await chromium.launchPersistentContext(profile, { channel: 'chrome', headless: true, baseURL: origin(frontend) });
    try { await use(context); } finally { await context.close(); }
  },
  page: async ({ context }, use, testInfo) => {
    const page = await context.newPage();
    try { await use(page); }
    finally {
      if (frontend === 'old') {
        const directory = resolve(output, 'baseline'); await mkdir(directory, { recursive: true });
        await page.screenshot({ path: resolve(directory, `${testInfo.testId}.png`), fullPage: true });
      }
      await page.close();
    }
  },
});
async function login(page) {
  await page.goto(frontend === 'new' ? '/probe/leptos/' : '/');
  await expect(page.locator('#username')).toBeVisible();
  await page.locator('#username').fill('probe'); await page.locator('#password').fill('probe-secret');
  await (frontend === 'new' ? page.locator('#login') : page.locator('form button[type="submit"]')).click();
  await expect.poll(() => page.evaluate(() => localStorage.getItem('auth-token'))).toBe(TOKEN);
}
// These are new transport/auth skeleton cases, never registrations in the baseline 72.
test('F2-SKELETON-001 login persists the shared auth-token', async ({ page }) => {
  await login(page);
  await expect.poll(async () => {
    const response = await page.request.get('/api/auth/user', { headers: { Authorization: `Bearer ${TOKEN}` } });
    return (await response.json()).user?.username;
  }).toBe('probe');
});
test('F2-SKELETON-002 UI requests authenticated project data', async ({ page }) => {
  const request = page.waitForRequest(req => new URL(req.url()).pathname === '/api/projects' && req.headers().authorization === `Bearer ${TOKEN}`);
  await login(page); await request;
  await expect(page.locator('body')).toContainText('F2 project');
});
test('F2-SKELETON-003 UI opens the authenticated main WebSocket', async ({ page }) => {
  let connected = false;
  page.on('websocket', socket => {
    const url = new URL(socket.url());
    if (url.pathname === '/ws' && url.searchParams.get('token') === TOKEN) {
      // A WS created event alone does not prove handshake succeeded; wait for a sent frame.
      socket.on('framesent', () => { connected = true; });
    }
  });
  await login(page);
  await expect.poll(() => connected, { timeout: 15000 }).toBe(true);
});
