// Same real account flow as baseline global setup, explicitly system Chrome.
import { requireUI, ownedPath } from './env.mjs';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
export default async function setup(config) {
  const { chromium, expect } = requireUI('@playwright/test');
  const project = config.projects[0], storageState = ownedPath(project.use.storageState);
  const browser = await chromium.launch({ channel: 'chrome', headless: true, ...project.use.launchOptions });
  try {
    const context = await browser.newContext(), page = await context.newPage();
    await page.goto(project.use.baseURL, { waitUntil: 'domcontentloaded' });
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
    if (process.env.PILOTDECK_E2E_MODE === 'ready') {
      // Real authenticated RPC read, not a fabricated project or UI response.
      const response = await page.evaluate(async () => {
        const reply = await fetch('/api/projects', { headers: { Authorization: `Bearer ${localStorage.getItem('auth-token')}` }, signal: AbortSignal.timeout(30_000) });
        return { status: reply.status, body: await reply.json() };
      });
      await writeFile(resolve(ownedPath(process.env.FE_OUTPUT), 'old-ready.projects.json'), JSON.stringify(response, null, 2) + '\n');
      expect(response.status, 'Native project RPC readiness').toBe(200);
      expect(Array.isArray(response.body)).toBeTruthy();
      const general = response.body.find(row => row.name === 'general');
      expect(general?.workspaceCwd).toBe(resolve(process.env.WORKSPACES_ROOT, 'general'));
      const seeded = response.body.filter(row => row.fullPath === resolve(process.env.WORKSPACES_ROOT, 'e2e-project'));
      expect(seeded, 'Native registered seed project (not merely a workspace directory)').toHaveLength(1);
      const runtime = JSON.parse(await readFile(ownedPath(process.env.PILOTDECK_E2E_RUNTIME_FILE), 'utf8'));
      expect(seeded[0].name).toBe(runtime.projectId);
      expect(seeded[0].kind).not.toBe('general');
      // Regular native rows omit capabilities; the UI enables files unless
      // explicitly false (General sets false). Do not require an invented field.
      expect(seeded[0].capabilities?.files !== false).toBe(true);
    }
    await context.storageState({ path: storageState });
  } finally { await browser.close(); }
}
