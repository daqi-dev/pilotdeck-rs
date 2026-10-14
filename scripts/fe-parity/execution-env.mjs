import { resolve } from 'node:path';
import { origin, realPorts, REAL_TEST_KEY } from './env.mjs';
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

// Real runtime receives an explicit isolated deployment, never an operator's one.
export function realExecutionEnv(parent, { output, runDir, mode }) {
  if (!['ready', 'onboarding'].includes(mode)) throw new Error('Invalid real mode');
  const env = executionEnv(parent, { frontend: 'old', output, suite: 'real' });
  for (const name of Object.keys(env)) if (/^(PILOT_|DATABASE_|WORKSPACES_ROOT$|HOST$|PORT$|SERVER_PORT$|VITE_|PROXY_|NODE_OPTIONS$|OPENAI_|ANTHROPIC_|GOOGLE_|GEMINI_|HTTP_PROXY$|HTTPS_PROXY$|ALL_PROXY$|NO_PROXY$|TASKMASTER_|NPM_CONFIG_)/i.test(name)) delete env[name];
  const ports = realPorts[mode];
  Object.assign(env, {
    BASELINE_UI: parent.BASELINE_UI || '',
    PILOTDECK_E2E_MODE: mode, PILOTDECK_E2E_RUN_DIR: runDir,
    PILOTDECK_E2E_RUNTIME_FILE: resolve(runDir, 'runtime.json'),
    PILOTDECK_E2E_LLM_CENTER_URL: `http://127.0.0.1:${realPorts.mock}`, PILOTDECK_E2E_API_KEY: REAL_TEST_KEY,
    PILOTDECK_E2E_SERVER_PORT: String(ports.server), PILOTDECK_E2E_VITE_PORT: String(ports.vite), PILOTDECK_E2E_GATEWAY_PORT: String(ports.gateway),
    PILOT_HOME: resolve(runDir, 'home'), DATABASE_PATH: resolve(runDir, 'auth.db'),
    WORKSPACES_ROOT: resolve(runDir, 'workspace'),
    PILOTDECK_CONFIG_PATH: resolve(runDir, 'pilotdeck.yaml'), PILOTDECK_DISABLE_LOCAL_AUTH: '0',
    SERVER_PORT: String(ports.server), VITE_PORT: String(ports.vite),
    HOST: '127.0.0.1', PROXY_HOST: '127.0.0.1', PILOTDECK_GATEWAY_HOST: '127.0.0.1',
    PILOTDECK_GATEWAY_PORT: String(ports.gateway), PILOTDECK_GATEWAY_URL: `ws://127.0.0.1:${ports.gateway}/ws`,
    // Match launch.mjs: gateway generates PILOT_HOME/server-token; bridge reads it.
    // An explicit TOKEN bypasses file creation and strands the file-only bridge.
    PILOTDECK_GATEWAY_TOKEN_PATH: resolve(runDir, 'home/server-token'),
    // Exclude operator-installed CLIs (task-master --version performs auto-update).
    PATH: `${resolve(runDir, 'bin')}:/usr/bin:/bin:/usr/sbin:/sbin`,
    TASKMASTER_SKIP_AUTO_UPDATE: '1',
    npm_config_offline: 'true', npm_config_cache: resolve(runDir, 'npm-cache'),
    VITE_IS_PLATFORM: 'false', NO_PROXY: '127.0.0.1',
  });
  return env;
}
