// #927 B-class new-side pairing config: the frozen baseline native-auth spec,
// verbatim, against the coexistence deployment (real native TS, switch on).
// The probe has no settings pages; an honest FAIL is the expected record until
// F3+ migration, and it never changes B scoring.
import { baseline, requireUI, ownedPath } from './env.mjs';
import { assertCoexistExecutionEnv, coexistOrigin } from './coexist-deploy.mjs';
import { realChromeLaunchOptions } from './real-browser.mjs';
assertCoexistExecutionEnv(process.env);
const output = ownedPath(process.env.FE_OUTPUT);
export default requireUI('@playwright/test').defineConfig({
  testDir: `${baseline}/e2e`, testMatch: 'real-auth-native.spec.mjs',
  forbidOnly: true, workers: 1, retries: 0, repeatEach: 1, fullyParallel: false,
  timeout: 30_000, expect: { timeout: 5_000 }, maxFailures: 0,
  metadata: { frontend: 'new', baseURL: coexistOrigin, suite: 'blackbox-deploy-B-new-side', backend: 'native-with-mock-llm' },
  reporter: [['json', { outputFile: `${output}/new-native-auth.json` }]],
  outputDir: `${output}/test-results/new-native-auth`,
  use: { baseURL: coexistOrigin, channel: 'chrome', headless: true,
    trace: 'on', screenshot: 'only-on-failure', video: 'off', serviceWorkers: 'block',
    launchOptions: realChromeLaunchOptions(process.env) },
});
