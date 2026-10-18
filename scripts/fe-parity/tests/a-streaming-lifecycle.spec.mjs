import { test, expect, aTitle, aEntry, open, step, stopStream, surface, top, pauseAtTop, historyReads } from './a-class.mjs';

// #1 and #5 rewrites of streaming-lifecycle.spec.mjs. Comments name the
// original line each statement replaces.
test.setTimeout(60000);
test.afterEach(stopStream);
const REFRESH = 'lifecycle-history-refresh', CANCEL = 'lifecycle-search-cancel';
// The compact_boundary row's visible label in MessagesPaneV2 (the fixture's pane too):
// a collapsed process attachment titled process.live.compactCompleted
// (processGrouping.ts:1201-1210, MessageRowV2.tsx:836-846). The legacy
// 'Context compacted' badge is MessageComponent.tsx:358-362, not chat-v2.
const TURN = ['Review this image', 'Thought process', 'Compacted context', 'The platform is too high.'];
// :31-32/:42 store kind order -> DOM order of the turn's visible labels. Missing,
// duplicated or reordered rows all change this list.
const turnOrder = page => surface(page).evaluate((node, labels) => {
  const text = node.textContent || '', hits = [];
  for (const label of labels) for (let at = text.indexOf(label); at >= 0; at = text.indexOf(label, at + 1)) hits.push([at, label]);
  return hits.sort((a, b) => a[0] - b[0]).map(([, label]) => label);
}, TURN);
// :31-32/:42 also fail on any extra store row; turnOrder alone cannot see a row
// sharing no text with TURN. Count rows (the baseline's own row selector,
// streaming-lifecycle.spec.mjs:128) outside and inside the expanded trace.
// Old side, derived from source: the compact segment's previous host is the
// thinking row (processGrouping.ts:377-389, :681-704), so it rides as that row's
// after-attachment (:864-886); the fold keeps prior-user, prior-answer, user,
// trace and answer (:944-969) and the trace holds only thinking
// (MessagesPaneV2.tsx:1110-1122). No windowing below 160 items (:127).
const ROWS = { rows: 5, traced: 1 };
const rowCounts = page => surface(page).evaluate(node => ({
  rows: [...node.querySelectorAll('[data-message-key]')].filter(row => !row.closest('[data-turn-trace]')).length,
  traced: node.querySelectorAll('[data-turn-trace] [data-message-key]').length,
}));
// complete triggers refreshFromServer; wait until the mock served the new history.
async function refreshed(page, after, persisted) {
  await expect.poll(async () => (await historyReads(page)).slice(after).some(read =>
    read.body.messages.some(message => message.id === persisted)), { timeout: 15000 }).toBe(true);
  await page.waitForTimeout(500); // Let the served history merge and render.
}
// After complete the turn folds into a completed trace (processGrouping.ts:909-969);
// the fixture kept it live (working=true). Open it by real input to see its rows.
async function expandTrace(page) {
  const header = surface(page).getByRole('button', { name: /^Processed/ });
  await expect(header).toHaveCount(1);
  if (await header.getAttribute('aria-expanded') !== 'true') await header.click();
  await expect(header).toHaveAttribute('aria-expanded', 'true');
}

test(aTitle(aEntry('streaming-lifecycle.spec.mjs', 'history refresh preserves thinking and compaction order and a single image query')), async ({ page }) => {
  const query = page.getByText('Review this image', { exact: true });
  const image = page.locator('img[src^="data:image/gif"]');
  await open(page, REFRESH, 'Earlier answer.');
  await step(page, REFRESH, 0); // :19-28 image query, echo, thinking, compaction, answer frames
  await expect(page.getByText('The platform is too high.', { exact: true })).toBeVisible();
  await expect(query).toHaveCount(1); // :29
  let reads = (await historyReads(page)).length;
  await step(page, REFRESH, 1); // :16-17, :30 complete -> refresh with the first history
  await refreshed(page, reads, 'persisted-answer');
  await expandTrace(page);
  await expect.poll(() => turnOrder(page)).toEqual(TURN); // :31-32
  await expect.poll(() => rowCounts(page)).toEqual(ROWS); // :31-32 no extra row
  await page.getByRole('button', { name: 'Thought process' }).click(); // :33
  await expect(page.getByText('Inspect the screenshot carefully.', { exact: true })).toBeVisible(); // :34
  await expect(query).toHaveCount(1); // :35
  await expect(image).toHaveCount(1); // :36
  const thoughtBox = await page.getByText('Inspect the screenshot carefully.', { exact: true }).boundingBox(); // :37
  const answerBox = await page.getByText('The platform is too high.', { exact: true }).boundingBox(); // :38
  expect(thoughtBox.y).toBeLessThan(answerBox.y); // :39
  reads = (await historyReads(page)).length;
  await step(page, REFRESH, 2); // :40-41 persisted compaction and full thinking, refresh again
  await refreshed(page, reads, 'persisted-compact');
  await expandTrace(page);
  await expect.poll(() => turnOrder(page)).toEqual(TURN); // :42
  await expect.poll(() => rowCounts(page)).toEqual(ROWS); // :42 no extra row
  await expect(query).toHaveCount(1); // :43
  await expect(image).toHaveCount(1); // :44
});

test(aTitle(aEntry('streaming-lifecycle.spec.mjs', 'stream search waits for the visible match and respects cancellation')), async ({ page }) => {
  const current = page.locator('mark[aria-current="true"]');
  await open(page, CANCEL, 'Read a long response');
  await pauseAtTop(page);
  await step(page, CANCEL, 0); // :97-100 open answer 'Starting.'
  await expect(surface(page)).toContainText('Starting.');
  await surface(page).click({ position: { x: 20, y: 100 } }); // :101
  await page.keyboard.press('Meta+f'); // :102
  await page.getByRole('searchbox').fill('TAIL_NEEDLE'); // :103
  await step(page, CANCEL, 1); // :104-107 long tail delta, stream still open
  await expect(current).toHaveCount(0); // :108
  await surface(page).hover({ position: { x: 20, y: 100 } }); // :110
  await page.mouse.wheel(0, -30); // :111
  await expect(current).toHaveText('TAIL_NEEDLE'); // :113
  expect(await top(surface(page))).toBe(0); // :114
});
