import { mkdir, mkdtemp } from 'node:fs/promises';
import { resolve } from 'node:path';
import { root, requireUI, origin, ownedPath } from '../env.mjs';
const { test: base, expect, chromium } = requireUI('@playwright/test');
export { expect };
export const frontend = process.env.FE_FRONTEND;
const output = ownedPath(process.env.FE_OUTPUT);
export const test = base.extend({
  context: async ({}, use) => {
    const directory = resolve(root, '_tmp/fe-parity/profiles');
    await mkdir(directory, { recursive: true });
    const profile = await mkdtemp(resolve(directory, `${frontend}-`));
    const context = await chromium.launchPersistentContext(profile, {
      channel: 'chrome', headless: true, baseURL: origin(frontend), viewport: { width: 1280, height: 900 },
    });
    try { await use(context); } finally { await context.close(); }
  },
  page: async ({ context }, use, testInfo) => {
    const page = await context.newPage();
    try { await use(page); }
    finally {
      if (frontend === 'old' && !page.isClosed()) {
        const directory = resolve(output, 'baseline'); await mkdir(directory, { recursive: true });
        await page.screenshot({ path: resolve(directory, `${testInfo.testId}.png`), fullPage: true }).catch(() => {});
      }
      await page.close();
    }
  },
});
export async function login(page) {
  await page.addInitScript(() => localStorage.setItem('userLanguage', 'en'));
  await page.goto(frontend === 'new' ? '/probe/leptos/' : '/');
  await expect(page.locator('#username')).toBeVisible();
  await page.locator('#username').fill('probe'); await page.locator('#password').fill('probe-secret');
  await (frontend === 'new' ? page.locator('#login') : page.locator('form button[type="submit"]')).click();
  await expect(page.locator('body')).toContainText('F2 project');
}
