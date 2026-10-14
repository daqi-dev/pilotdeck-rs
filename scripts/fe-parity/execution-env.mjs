import { resolve } from 'node:path';
import { origin } from './env.mjs';
export const PROFILES = Object.freeze(['staffdeck', 'minimal', 'replacement']);
// Do not inherit an operator's deployment URL, profile, API key or runtime path.
// Baseline specs consult these variables directly, bypassing use.baseURL.
export function executionEnv(parent, { frontend, output, profile, suite } = {}) {
  const env = Object.fromEntries(Object.entries(parent).filter(([name]) =>
    !/^(FORMAL_|REAL_|PILOTDECK_|REPLACEMENT_|FE_|PLAYWRIGHT_)/.test(name)));
  env.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = '1';
  if (!frontend) return env; // inventory has exactly the original undefined route title
  Object.assign(env, { FE_FRONTEND: frontend, FE_OUTPUT: output, FE_SUITE: suite,
    TMPDIR: resolve(output, 'browser-tmp'), TMP: resolve(output, 'browser-tmp'), TEMP: resolve(output, 'browser-tmp') });
  if (suite === 'baseline') {
    if (!PROFILES.includes(profile)) throw new Error('Unknown baseline profile');
    Object.assign(env, {
      FE_PROFILE: profile, REAL_COMPOSITION_PROFILE: profile, REAL_COMPOSITION_URL: origin(frontend),
      REAL_NATIVE_URL: origin(frontend), FORMAL_COMPOSITION_URL: origin(frontend),
      FORMAL_REPLACEMENT: profile === 'replacement' ? '1' : '0',
      // FORMAL_ROUTE_PROFILE intentionally unset: changing its expanded title is a ruler change.
      PILOTDECK_API_URL: origin(frontend), PILOTDECK_E2E_PROJECT_PATH: '/probe', PILOTDECK_E2E_PARENT_SESSION: 's',
      PILOTDECK_E2E_API_KEY: 'f2-mock-key', PILOTDECK_E2E_RUNTIME_FILE: resolve(output, 'mock-runtime.json'),
      REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH: resolve(output, 'mock-replacement.ndjson'),
    });
  }
  return env;
}
