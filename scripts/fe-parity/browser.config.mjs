import { root, requireUI, origin, ownedPath } from './env.mjs';
const frontend = process.env.FE_FRONTEND;
if (!['old', 'new'].includes(frontend)) throw new Error('FE_FRONTEND must be old or new');
const output = ownedPath(process.env.FE_OUTPUT);
const testMatch = { 'streaming-scroll': 'streaming-scroll.spec.mjs', 'a-class': 'a-*.spec.mjs' }[process.env.FE_SUITE] || 'skeleton.spec.mjs';
export default requireUI('@playwright/test').defineConfig({
  testDir: `${root}/scripts/fe-parity/tests`, testMatch,
  forbidOnly: true, workers: 1, retries: 0, timeout: 30000,
  metadata: { frontend, baseURL: origin(frontend) },
  reporter: [['json', { outputFile: `${output}/${frontend}.json` }]],
  outputDir: `${output}/test-results/${frontend}`,
  use: { baseURL: origin(frontend), channel: 'chrome', headless: true, screenshot: 'off', video: 'off', trace: 'off' },
});
