// Reuse the immutable specs only. Never import the baseline's config/setup/launch.
import { baseline, requireUI, origin, ownedPath } from './env.mjs';
import { PROFILES } from './execution-env.mjs';
import { TOKEN } from './mock.mjs';
const frontend = process.env.FE_FRONTEND, profile = process.env.FE_PROFILE;
if (!['old', 'new'].includes(frontend) || !PROFILES.includes(profile)) throw new Error('Invalid baseline frontend/profile');
const output = ownedPath(process.env.FE_OUTPUT);
export default requireUI('@playwright/test').defineConfig({
  testDir: `${baseline}/e2e`, testMatch: '**/*.spec.mjs',
  forbidOnly: true, workers: 1, retries: 0, repeatEach: 1, fullyParallel: false,
  timeout: 30000, expect: { timeout: 5000 }, maxFailures: 0,
  metadata: { frontend, baseURL: origin(frontend), profile, backend: 'f2-mock', suite: 'baseline-72' },
  reporter: [['json', { outputFile: `${output}/${frontend}-${profile}.json` }]],
  outputDir: `${output}/test-results/${frontend}-${profile}`,
  use: {
    baseURL: origin(frontend), channel: 'chrome', headless: true,
    viewport: { width: 1280, height: 900 }, screenshot: 'off', video: 'off', trace: 'off',
    // Chromium creates its temporary user-data-dir beneath worktree TMPDIR.
    launchOptions: { env: { ...process.env, TMPDIR: `${output}/browser-tmp`, TMP: `${output}/browser-tmp`, TEMP: `${output}/browser-tmp` } },
    storageState: { cookies: [], origins: [{ origin: origin(frontend), localStorage: [
      { name: 'auth-token', value: TOKEN }, { name: 'userLanguage', value: 'en' },
    ] }] },
    extraHTTPHeaders: { Authorization: `Bearer ${TOKEN}` },
  },
});
