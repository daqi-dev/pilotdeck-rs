import { root, requireUI, ownedPath } from './env.mjs';
import { assertCoexistExecutionEnv, coexistOrigin } from './coexist-deploy.mjs';
import { realChromeLaunchOptions } from './real-browser.mjs';
assertCoexistExecutionEnv(process.env);
const phase = process.env.FE_COEXIST_PHASE, output = ownedPath(process.env.FE_OUTPUT);
export default requireUI('@playwright/test').defineConfig({
  testDir: `${root}/scripts/fe-parity/coexist-tests`, testMatch: /coexist\.spec\.mjs$/,
  forbidOnly: true, workers: 1, retries: 0, repeatEach: 1, fullyParallel: false,
  timeout: 120_000, expect: { timeout: 15_000 }, maxFailures: 0,
  metadata: { frontend: 'old', baseURL: coexistOrigin, phase, suite: 'coexist-F3', backend: 'native-with-mock-llm' },
  reporter: [['json', { outputFile: `${output}/coexist-${phase}.json` }]],
  outputDir: `${output}/test-results/coexist-${phase}`,
  use: { baseURL: coexistOrigin, channel: 'chrome', headless: true,
    trace: 'on', screenshot: 'only-on-failure', video: 'off', serviceWorkers: 'block',
    launchOptions: realChromeLaunchOptions(process.env) },
});
