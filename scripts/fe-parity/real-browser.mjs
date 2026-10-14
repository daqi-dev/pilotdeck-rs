import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { requireUI } from './env.mjs';
import { realChromeEnv } from './execution-env.mjs';

export const requiredChromeFlags = Object.freeze(['--disable-component-update', '--disable-background-networking', '--no-first-run', '--no-default-browser-check']);
// Resolve through test -> playwright -> core: pnpm may contain multiple core
// versions, and requireUI('playwright-core') need not be the test runner's copy.
export function chromeDefaultsEvidence() {
  const testRequire = createRequire(requireUI.resolve('@playwright/test'));
  const playwrightRequire = createRequire(testRequire.resolve('playwright'));
  const packagePath = playwrightRequire.resolve('playwright-core/package.json');
  const directory = dirname(packagePath);
  const sourcePath = resolve(directory, 'lib/coreBundle.js');
  const source = readFileSync(sourcePath, 'utf8');
  const switches = /chromiumSwitches = \([^)]*\) => \[([\s\S]*?)\n\s*\]\.filter\(Boolean\)/.exec(source)?.[1];
  if (!switches) throw new Error('FAIL: cannot verify installed Playwright Chrome defaults');
  const defaults = [...switches.matchAll(/^\s*"(--[^"\n]+)",?\s*$/gm)].map(match => match[1]);
  return { version: JSON.parse(readFileSync(packagePath, 'utf8')).version, sourcePath,
    sourceSHA256: createHash('sha256').update(source).digest('hex'),
    defaults: requiredChromeFlags.filter(flag => defaults.includes(flag)),
    appended: requiredChromeFlags.filter(flag => !defaults.includes(flag)) };
}
export function realChromeLaunchOptions(serviceEnv) {
  const evidence = chromeDefaultsEvidence();
  return { args: ['--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1', ...evidence.appended],
    env: realChromeEnv(serviceEnv) };
}
