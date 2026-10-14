import { test, expect, login } from './fixture.mjs';
import { TOKEN } from '../mock.mjs';
// These are new transport/auth skeleton cases, never registrations in the baseline 72.
test('F2-SKELETON-001 login persists the shared auth-token', async ({ page }) => {
  await login(page);
  await expect.poll(() => page.evaluate(() => localStorage.getItem('auth-token'))).toBe(TOKEN);
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
