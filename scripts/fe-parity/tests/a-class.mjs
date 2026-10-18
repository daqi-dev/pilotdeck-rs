import { test, expect, login, frontend } from './fixture.mjs';
import { A_IDENTITIES, aTitle } from '../accounting.mjs';
export { test, expect, aTitle };

// A rewrites drive only real input plus mock wire frames (stream.mjs A_SCRIPTS).
// Observations are DOM text/role/geometry/scrollTop and mock network reads;
// no frontend store, hook or fixture-only element is touched.
// Specs call test(aTitle(aEntry(...))) themselves so Playwright records the
// spec file as the registration location, as for the B and D suites.
export function aEntry(file, title) {
  // Unknown or drifted source identities fail at --list, before any browser.
  const entry = A_IDENTITIES.find(row => row.file === file && row.title === title);
  if (!entry) throw new Error(`Unknown A identity ${file} :: ${title}`);
  return entry;
}
export const surface = page => page.locator('[data-chat-search-surface]').first();
export const top = locator => locator.evaluate(node => node.scrollTop);
export const gap = locator => locator.evaluate(node => node.scrollHeight - node.clientHeight - node.scrollTop);
export const latest = page => page.getByRole('button', { name: /Back to latest|回到最新|返回最新/i });
const control = (page, query) => page.request.post(`/__parity/stream?${new URLSearchParams(query)}`);
export const streamState = async page => (await page.request.get('/__parity/stream')).json();
export const historyReads = async page => (await (await page.request.get('/__parity/stats')).json()).historyResponses;
export const stopStream = async ({ page }) => { await control(page, { action: 'stop' }); };
export async function open(page, script, lastText) {
  expect((await control(page, { action: 'reset', script })).ok()).toBe(true);
  await login(page);
  await page.goto('/session/s');
  await expect(surface(page), frontend === 'new' ? 'NOT_IMPLEMENTED: probe has no conversation scroll/search surface' : 'old conversation must render').toBeVisible({ timeout: 15000 });
  await expect(surface(page).getByText(lastText, { exact: true })).toBeAttached();
  await expect.poll(async () => (await streamState(page)).watchers).toBe(1);
  await expect.poll(() => gap(surface(page))).toBeLessThan(3);
}
export async function step(page, script, index) {
  const response = await control(page, { action: 'step', script, step: String(index) });
  expect(response.status(), await response.text()).toBe(202);
  const { recipients } = await response.json();
  expect(recipients.length > 0 && recipients.every(count => count === 1), 'each frame must reach exactly one watched browser').toBe(true);
}
// The baseline transcript fixture has no scroll follower and starts at the top.
// Reach that state through real input in the full shell, as F2-STREAM-SEARCH does.
export async function pauseAtTop(page) {
  await surface(page).click({ position: { x: 20, y: 250 } });
  await page.keyboard.press('Home');
  await page.waitForTimeout(200); // Settle the actual keyboard gesture.
  await expect.poll(() => top(surface(page))).toBe(0);
  await expect(latest(page)).toBeVisible();
}
