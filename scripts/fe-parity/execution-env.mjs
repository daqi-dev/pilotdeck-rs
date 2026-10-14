import { resolve, dirname } from 'node:path';
import { userInfo } from 'node:os';
import { mkdir, realpath } from 'node:fs/promises';
import { origin, realPorts, REAL_TEST_KEY } from './env.mjs';
export const PROFILES = Object.freeze(['staffdeck', 'minimal', 'replacement']);
// Only the native browser receives the account HOME; its Node driver and every
// process importing the baseline keep assertRealExecutionEnv's isolated HOME.
export function realChromeEnv(serviceEnv) {
  assertRealExecutionEnv(serviceEnv);
  const env = { ...serviceEnv, HOME: userInfo().homedir, USERPROFILE: userInfo().homedir, NODE_OPTIONS: '' };
  for (const name of ['XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME']) delete env[name];
  return env;
}
export function realOSPaths(runDir) {
  return { HOME: resolve(runDir, 'home-os'), USERPROFILE: resolve(runDir, 'home-os'),
    XDG_CONFIG_HOME: resolve(runDir, 'home-os/.config'), XDG_DATA_HOME: resolve(runDir, 'home-os/.local/share'),
    XDG_CACHE_HOME: resolve(runDir, 'home-os/.cache'),
    TMPDIR: resolve(runDir, 'tmp'), TMP: resolve(runDir, 'tmp'), TEMP: resolve(runDir, 'tmp') };
}
// userInfo reads the OS account record, not the inherited (possibly overridden)
// HOME. This assertion must run before any tested process is spawned/imported.
export function assertRealExecutionEnv(env) {
  if (!env.HOME || resolve(env.HOME) === resolve(userInfo().homedir)) throw new Error('FAIL: real HOME equals operator home or is missing');
  if (!env.PILOTDECK_E2E_RUN_DIR) throw new Error('FAIL: missing isolated real run directory');
  for (const [name, expected] of Object.entries(realOSPaths(env.PILOTDECK_E2E_RUN_DIR))) {
    if (env[name] !== expected) throw new Error(`FAIL: real ${name} must equal ${expected}`);
  }
  if (env.PILOT_HOME !== resolve(env.PILOTDECK_E2E_RUN_DIR, 'home')) throw new Error('FAIL: PILOT_HOME must be separate from OS HOME');
  if (env.PILOTDECK_SKIP_BROWSER_OPEN !== '1') throw new Error('FAIL: real requires PILOTDECK_SKIP_BROWSER_OPEN=1');
}
export async function prepareRealOSDirectories(env) {
  assertRealExecutionEnv(env);
  for (const directory of new Set(Object.values(realOSPaths(env.PILOTDECK_E2E_RUN_DIR)))) {
    for (let ancestor = directory; ; ancestor = dirname(ancestor)) {
      try {
        if (await realpath(ancestor) !== ancestor) throw new Error('FAIL: real OS directory has a symlink escape');
        break;
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    await mkdir(directory, { recursive: true, mode: 0o700 });
    if (await realpath(directory) !== directory) throw new Error('FAIL: real OS directory has a symlink escape');
  }
}
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
  for (const name of Object.keys(env)) if (/^(PILOT_|DATABASE_|WORKSPACES_ROOT$|HOST$|PORT$|SERVER_PORT$|VITE_|PROXY_|NODE_OPTIONS$|OPENAI_|ANTHROPIC_|GOOGLE_|GEMINI_|HTTP_PROXY$|HTTPS_PROXY$|ALL_PROXY$|NO_PROXY$|TASKMASTER_|NPM_CONFIG_|GIT_)/i.test(name)) delete env[name];
  // LibreOffice bootstrap accepts environment overrides before defaults.
  // Its --version probe must resolve the installed default through isolated HOME.
  for (const name of ['UserInstallation', 'SYSUSERCONFIG', 'SYSUSERHOME', 'URE_BOOTSTRAP']) delete env[name];
  const ports = realPorts[mode];
  Object.assign(env, {
    ...realOSPaths(runDir), PILOTDECK_SKIP_BROWSER_OPEN: '1',
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
  assertRealExecutionEnv(env);
  return env;
}
