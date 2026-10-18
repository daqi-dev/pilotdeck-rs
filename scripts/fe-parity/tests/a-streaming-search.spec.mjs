import { test, expect, aTitle, aEntry, open, step, stopStream, surface, top, gap, pauseAtTop } from './a-class.mjs';

// #15 rewrite of streaming-search.spec.mjs:21 (first scenario, :17). Comments
// name the original line each statement replaces.
test.setTimeout(60000);
test.afterEach(stopStream);
const SCRIPT = 'search-text-link';
async function search(page, query) { // :10-14
  await surface(page).click({ position: { x: 20, y: 100 } });
  await page.keyboard.press('Meta+f');
  await page.getByRole('searchbox').fill(query);
}

test(aTitle(aEntry('streaming-search.spec.mjs', 'search locates a phrase spanning text and a link')), async ({ page }) => {
  // :22-25 history(30) + target now come from the mock history endpoint.
  await open(page, SCRIPT, 'Visit https://example.com/unique-guide now.');
  const link = page.getByRole('link', { name: 'https://example.com/unique-guide', exact: true });
  await pauseAtTop(page);
  // Precondition: search, not the initial bottom position, brings the link in.
  await expect(link).not.toBeInViewport();
  await search(page, 'Visit https://example.com/unique-guide'); // :26
  await expect(link).toBeInViewport(); // :27
  await expect(page.locator('mark[aria-current="true"]')).toHaveCount(0); // :28
  await surface(page).hover({ position: { x: 20, y: 100 } }); // :29
  await page.mouse.wheel(0, -160); // :30
  await expect.poll(() => gap(surface(page))).toBeGreaterThan(50); // :31
  const reading = await top(surface(page)); // :32
  await step(page, SCRIPT, 0); // :33-35 one static answer frame
  await expect(surface(page)).toContainText('Another response.');
  await page.waitForTimeout(250); // :36
  expect(Math.abs(await top(surface(page)) - reading)).toBeLessThan(2); // :37
});
