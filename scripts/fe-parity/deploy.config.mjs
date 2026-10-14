import { baseline, requireUI, ownedPath } from './env.mjs';
import { assertDeployExecutionEnv, deployCell, deployOrigin, formalProjects } from './deploy-matrix.mjs';
import { realChromeLaunchOptions } from './real-browser.mjs';
assertDeployExecutionEnv(process.env);
const cell = deployCell(process.env.FE_B_CELL), output = ownedPath(process.env.FE_OUTPUT);
export default requireUI('@playwright/test').defineConfig({
  testDir: `${baseline}/e2e`, testMatch: cell.files,
  forbidOnly: true, workers: 1, retries: 0, repeatEach: 1, fullyParallel: false,
  // B's frozen configs use Playwright's default 30s / 5s timeouts.
  timeout: 30_000, expect: { timeout: 5_000 }, maxFailures: 0,
  projects: ['formal', 'routes'].includes(cell.kind) ? formalProjects : [{ name: '' }],
  metadata: { frontend: 'old', baseURL: deployOrigin, profile: cell.profile, cell: cell.id,
    suite: 'blackbox-deploy-B', backend: 'native-with-mock-llm' },
  reporter: [['json', { outputFile: `${output}/old-${cell.id}.json` }]],
  outputDir: `${output}/test-results/old-${cell.id}`,
  // Auth composition specs do not complete onboarding; prepare their own
  // fixed test account through real APIs before unchanged specs authenticate.
  globalSetup: new URL('./deploy-setup.mjs', import.meta.url).pathname,
  use: { baseURL: deployOrigin, channel: 'chrome', headless: true,
    trace: 'on', screenshot: 'only-on-failure', video: 'off', serviceWorkers: 'block',
    launchOptions: realChromeLaunchOptions(process.env) },
});
