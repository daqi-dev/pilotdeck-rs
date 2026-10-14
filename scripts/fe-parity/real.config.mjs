import { baseline, root, requireUI, ownedPath, realOrigin } from './env.mjs';
import { assertRealExecutionEnv } from './execution-env.mjs';
import { realChromeLaunchOptions } from './real-browser.mjs';
assertRealExecutionEnv(process.env);
const output = ownedPath(process.env.FE_OUTPUT), runDir = ownedPath(process.env.PILOTDECK_E2E_RUN_DIR);
const mode = process.env.PILOTDECK_E2E_MODE;
if (process.env.FE_FRONTEND !== 'old' || !['ready', 'onboarding'].includes(mode)) throw new Error('Real native runtime currently supports old only');
export default requireUI('@playwright/test').defineConfig({
  testDir: mode === 'ready' ? `${root}/scripts/fe-parity/real-tests` : `${baseline}/e2e`,
  testMatch: mode === 'ready' ? /[^/]+\.spec\.mjs$/ : /real\/onboarding\.spec\.mjs$/,
  forbidOnly: true, workers: 1, retries: 0, repeatEach: 1, fullyParallel: false,
  timeout: 120_000, expect: { timeout: 15_000 }, maxFailures: 0,
  metadata: { frontend: 'old', baseURL: realOrigin(mode), mode, backend: 'native-with-mock-llm', suite: 'real-D' },
  reporter: [['json', { outputFile: `${output}/old-${mode}.json` }]],
  outputDir: `${output}/test-results/old-${mode}`,
  globalSetup: new URL('./real-setup.mjs', import.meta.url).pathname,
  use: { baseURL: realOrigin(mode), storageState: `${runDir}/storage-state.json`, channel: 'chrome', headless: true,
    viewport: { width: 1440, height: 900 }, trace: 'retain-on-failure', screenshot: 'only-on-failure',
    // Video would require Playwright's separate downloaded ffmpeg. Use trace/screenshots.
    video: 'off', serviceWorkers: 'block', launchOptions: realChromeLaunchOptions(process.env),
  },
});
