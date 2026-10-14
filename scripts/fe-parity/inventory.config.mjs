// --list only: no webServer, browser, deployment, or source mutation.
import { baseline, requireUI, root } from './env.mjs';
export default requireUI('@playwright/test').defineConfig({
  testDir: `${baseline}/e2e`, testMatch: '**/*.spec.mjs', forbidOnly: true,
  workers: 1, retries: 0, reporter: 'json', outputDir: `${root}/_tmp/fe-parity/inventory-output`,
});
