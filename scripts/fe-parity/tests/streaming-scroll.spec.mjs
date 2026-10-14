import { test, expect, login, frontend } from './fixture.mjs';
import { STREAM_SCENARIOS, STREAM_PROTOCOLS, marker, streamErrors, textErrors, readingErrors, followErrors } from '../stream.mjs';

// All product observations are DOM text/geometry/scrollTop. __f2Observation is
// test-owned DOM sampling state; it never reads React/Rust stores or WS frames.
test.setTimeout(60000);
const surface = page => page.locator('[data-chat-search-surface]').first();
const metrics = pane => pane.evaluate(node => ({ top: node.scrollTop, distance: node.scrollHeight - node.clientHeight - node.scrollTop, height: node.scrollHeight }));
const control = (page, action, scenario = '', protocol = 'legacy') => page.request.post(`/__parity/stream?action=${action}&scenario=${scenario}&protocol=${protocol}`);
const state = async page => (await page.request.get('/__parity/stream')).json();
async function open(page) {
  expect((await control(page, 'reset')).ok()).toBe(true);
  await login(page);
  await page.goto('/session/s');
  await expect(surface(page), frontend === 'new' ? 'NOT_IMPLEMENTED: probe has no conversation scroll/search surface' : 'old conversation must render').toBeVisible({ timeout: 15000 });
  await expect(page.getByText('Question 23', { exact: true })).toBeVisible();
  await expect.poll(async () => (await state(page)).watchers).toBe(1);
  await expect.poll(async () => (await metrics(surface(page))).distance).toBeLessThan(3);
}
async function observe(page) {
  await surface(page).evaluate(node => {
    const record = { samples: [], regressions: [], stopped: false, lastCount: 0, text: '' };
    globalThis.__f2Observation = record;
    const sample = () => {
      const text = node.innerText;
      const count = [...text.matchAll(/F2FRAME\d{4}END/g)].length;
      if (count < record.lastCount) record.regressions.push({ before: record.lastCount, after: count });
      record.lastCount = count; record.text = text;
      record.samples.push({ at: performance.now(), count, top: node.scrollTop, distance: node.scrollHeight - node.clientHeight - node.scrollTop });
      if (!record.stopped) requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  });
}
async function start(page, scenario, protocol) { expect((await control(page, 'start', scenario, protocol)).status()).toBe(202); }
async function attachResumeEvidence(page, testInfo) {
  // Keep the final assertion input and scroll geometry, including on failure.
  const dom = await surface(page).evaluate(node => {
    // Read both in one DOM evaluation without scrolling or forcing rows to render.
    const innerText = node.innerText, textContent = node.textContent || '';
    const markers = innerText.match(/F2FRAME\d{4}END/g) || [];
    const contentMarkers = textContent.match(/F2FRAME\d{4}END/g) || [];
    const firstRoundParagraphs = [...node.querySelectorAll('p')].flatMap(paragraph => {
      const markers = (paragraph.textContent || '').match(/F2FRAME\d{4}END/g) || [];
      if (!markers.some(token => Number(token.slice(7, 11)) < 200)) return [];
      const row = paragraph.closest('.chat-message[data-message-key]');
      return [{ markers, inDOM: paragraph.isConnected && node.contains(paragraph),
        contentVisibility: getComputedStyle(paragraph).contentVisibility,
        rowKey: row?.getAttribute('data-message-key'),
        rowContentVisibility: row ? getComputedStyle(row).contentVisibility : null }];
    });
    return { text: innerText, innerText, textContent,
      innerTextCount: markers.length, uniqueMarkerCount: new Set(markers).size,
      innerTextUniqueMarkerCount: new Set(markers).size,
      textContentCount: contentMarkers.length, textContentUniqueMarkerCount: new Set(contentMarkers).size,
      firstRoundParagraphCount: firstRoundParagraphs.length, firstRoundParagraphs,
      top: node.scrollTop, height: node.scrollHeight, viewport: node.clientHeight,
      distance: node.scrollHeight - node.clientHeight - node.scrollTop,
      rows: [...node.querySelectorAll('.chat-message[data-message-key]')].map(row => ({
        key: row.getAttribute('data-message-key'),
        markers: (row.textContent || '').match(/F2FRAME\d{4}END/g) || [],
        contentVisibility: getComputedStyle(row).contentVisibility,
      })),
    };
  });
  await testInfo.attach('resume-final-evidence', {
    body: JSON.stringify({ injection: await state(page), dom }), contentType: 'application/json',
  });
}
async function finish(page, scenario, testInfo) {
  await expect.poll(async () => (await state(page)).status, { timeout: 85000, intervals: [200] }).toBe('completed');
  await expect.poll(async () => (await surface(page).innerText()).includes(marker(STREAM_SCENARIOS[scenario] - 1)), { timeout: 10000 }).toBe(true);
  // Include completion/history refresh layout in the observation window.
  await page.waitForTimeout(700);
  const observation = await page.evaluate(() => { globalThis.__f2Observation.stopped = true; return globalThis.__f2Observation; });
  const injection = await state(page);
  await testInfo.attach('stream-dom-evidence', { body: JSON.stringify({ injection, observation }), contentType: 'application/json' });
  expect(streamErrors(injection)).toEqual([]);
  expect(textErrors(observation.text, STREAM_SCENARIOS[scenario])).toEqual([]);
  expect(observation.regressions).toEqual([]);
  expect(new Set(observation.samples.map(sample => sample.count)).size, 'must render many intermediate updates, not just a completed snapshot').toBeGreaterThan(20);
  return observation;
}
async function readUp(page, delta = -120) {
  const pane = surface(page), before = (await metrics(pane)).top;
  await pane.hover({ position: { x: 20, y: 250 } });
  await page.mouse.wheel(0, delta);
  await expect.poll(async () => (await metrics(pane)).top).toBeLessThan(before - 10);
  await expect(page.getByRole('button', { name: /Back to latest|回到最新|返回最新/i })).toBeVisible();
  await page.waitForTimeout(200); // Let the real wheel/scroll gesture settle before measuring.
  return (await metrics(pane)).top;
}
test.afterEach(async ({ page }) => { await control(page, 'stop'); });

for (const protocol of STREAM_PROTOCOLS) {

  test(`F2-STREAM-001 [${protocol}] 25Hz for 60s retains every delta and follows the bottom`, async ({ page }, testInfo) => {
    test.setTimeout(110000);
    await open(page); await observe(page); await start(page, 'endurance', protocol);
    // Check throughout the stream, allowing one layout/follow turn to settle.
    for (let i = 0; i < 12; i++) {
      await page.waitForTimeout(4500);
      await expect.poll(async () => (await metrics(surface(page))).distance, { timeout: 1000, intervals: [40] }).toBeLessThan(3);
    }
    const observation = await finish(page, 'endurance', testInfo);
    expect(followErrors(observation.samples)).toEqual([]);
  });

  test(`F2-STREAM-002 [${protocol}] reading does not jump during deltas or completion and explicit resume follows`, async ({ page }, testInfo) => {
    await open(page);
    const readingTop = await readUp(page);
    await observe(page); await start(page, 'reading', protocol);
    const observation = await finish(page, 'reading', testInfo);
    expect(readingErrors(observation.samples, readingTop)).toEqual([]);
    await page.getByRole('button', { name: /Back to latest|回到最新|返回最新/i }).click();
    await expect.poll(async () => (await metrics(surface(page))).distance).toBeLessThan(3);
    // Resume must keep following subsequent growth, not just perform one scroll.
    const previousHeight = (await metrics(surface(page))).height;
    await start(page, 'reading', protocol);
    try {
      await expect.poll(async () => (await metrics(surface(page))).height).toBeGreaterThan(previousHeight + 100);
      await expect.poll(async () => (await state(page)).status, { timeout: 15000 }).toBe('completed');
      expect(streamErrors(await state(page))).toEqual([]);
      // DOM retention includes offscreen content skipped by content-visibility:auto.
      // innerText measures rendered text and can omit the entire first answer.
      await expect.poll(async () => textErrors(await surface(page).textContent() || '', 2 * STREAM_SCENARIOS.reading)).toEqual([]);
      await expect.poll(async () => textErrors(await surface(page).innerText(), STREAM_SCENARIOS.reading, STREAM_SCENARIOS.reading)).toEqual([]);
      await expect.poll(async () => (await metrics(surface(page))).distance).toBeLessThan(3);
    } finally {
      await attachResumeEvidence(page, testInfo);
    }
  });

  for (const query of ['F2 searchable lighthouse', 'f2-hidden-reference']) {
    test(`F2-STREAM-SEARCH [${protocol}] locates ${query} and does not recenter while reading`, async ({ page }, testInfo) => {
      await open(page);
      await surface(page).click({ position: { x: 20, y: 250 } });
      // TranscriptFixture starts at the top without the shell's scroll follower.
      // Establish that reading precondition through real input in the full shell:
      // Home pauses following and lets its reading anchor settle before search.
      await page.keyboard.press('Home');
      await page.waitForTimeout(200); // Settle the actual keyboard gesture, as readUp does for wheel.
      await expect.poll(async () => (await metrics(surface(page))).top).toBeLessThan(3);
      await expect(page.getByRole('button', { name: /Back to latest|回到最新|返回最新/i })).toBeVisible();
      const seedTarget = surface(page).getByRole('link', { name: 'F2 reference', exact: true });
      await expect(seedTarget).not.toBeInViewport();
      const beforeSearch = await metrics(surface(page));
      await page.keyboard.press('Meta+f');
      await expect(page.getByRole('searchbox')).toBeVisible();
      await page.getByRole('searchbox').fill(query);
      const search = page.getByRole('search', { name: 'Search in conversation' });
      await expect(search.getByText('1 / 1', { exact: true })).toBeVisible();
      await expect(search.getByRole('button', { name: 'Next match', exact: true })).toBeEnabled();
      const target = query === 'f2-hidden-reference' ? surface(page).getByRole('link', { name: 'F2 reference', exact: true }) : surface(page).locator('mark[aria-current="true"]').filter({ hasText: query });
      try {
        await expect(target).toHaveCount(1);
        await expect(target).toBeInViewport();
        await expect.poll(async () => (await metrics(surface(page))).distance).toBeGreaterThan(50);
        if (query === 'f2-hidden-reference') await expect(target).toHaveAttribute('href', 'https://example.invalid/f2-hidden-reference');
      } finally {
        // Also retain failed navigation geometry; never scroll the target in test code.
        await testInfo.attach('search-dom-evidence', { body: JSON.stringify({ protocol, query, beforeSearch,
          afterSearch: await surface(page).evaluate((pane, query) => {
            const target = query === 'f2-hidden-reference'
              ? pane.querySelector('a[href="https://example.invalid/f2-hidden-reference"]')
              : pane.querySelector('mark[aria-current="true"]');
            return { target: target?.getBoundingClientRect().toJSON() ?? null,
              pane: pane.getBoundingClientRect().toJSON(), top: pane.scrollTop,
              height: pane.scrollHeight, viewport: pane.clientHeight,
              distance: pane.scrollHeight - pane.clientHeight - pane.scrollTop,
              virtualized: Boolean(pane.querySelector('[data-virtualized-messages]')),
              paused: pane.dataset.scrollFollowPaused, readingAnchor: pane.dataset.readingAnchorKey };
          }, query) }), contentType: 'application/json' });
      }
      const readingTop = await readUp(page);
      await observe(page); await start(page, 'search', protocol);
      const observation = await finish(page, 'search', testInfo);
      expect(readingErrors(observation.samples, readingTop)).toEqual([]);
      await page.getByRole('searchbox').press('Escape');
      await expect(page.getByRole('searchbox')).toBeHidden();
    });
  }

}
