// #927 F3 coexistence deployment: repository-owned assembly of a native TS
// deployment copy that hosts both shells behind one origin (Plan 004 F3, Option
// A). The read-only baseline stays untouched: the A patch, the new-shell assets
// and the boundary injection live only in the copy under _build/.
import { execFile } from 'node:child_process';
import { copyFile, cp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { baseline, newDist, oldDist, ownedPath, realPorts, requireUI, root, REAL_TEST_KEY } from './env.mjs';
import { realExecutionEnv, assertRealExecutionEnv } from './execution-env.mjs';

const run = promisify(execFile);

export const coexistPorts = Object.freeze({ server: 28420, gateway: 28421, vite: 28422 });
export const coexistOrigin = `http://127.0.0.1:${coexistPorts.server}`;
export const coexistRunRoot = resolve(root, '_tmp/fe-parity/coexist-run');
export const coexistDeployRoot = resolve(root, '_build/fe-parity/coexist-deploy');
export const coexistCopyRoot = resolve(coexistDeployRoot, 'pilotdeck');
export const repoBaseline = resolve(baseline, '..');
export const coexistAllowedPorts = Object.freeze([realPorts.mock, ...Object.values(coexistPorts)]);
export const PHASES = Object.freeze(['valid', 'invalid', 'rollback']);
// The migrated prefix. Strict segment boundary everywhere it is matched.
export const NEW_SHELL_PREFIX = '/settings';
export const NEW_SHELL_MOUNT = '/probe/leptos';
// The new bundle's own entrypoint script, referenced only by the new shell's
// index.html (boundary-boot.js is injected into BOTH shells, so it cannot
// discriminate). S1 asserts on this marker; buildNewShellDirectory enforces it.
export const NEW_SHELL_ENTRY_MARKER = `${NEW_SHELL_MOUNT}/probe.js`;

export function coexistExecutionEnv(parent, { output, runDir, phase, frontend = 'old' }) {
  if (!PHASES.includes(phase)) throw new Error('Invalid coexist phase');
  if (!['old', 'new'].includes(frontend)) throw new Error('Invalid coexist frontend');
  // Reuse D's sanitization and OS isolation; override every endpoint to the
  // coexistence deployment (origin = server port: start-built serves the SPA).
  const env = realExecutionEnv(parent, { output, runDir, mode: 'ready' });
  Object.assign(env, { FE_SUITE: 'coexist', FE_COEXIST_PHASE: phase, FE_FRONTEND: frontend,
    SKIP_BROWSER_OPEN: '1', PILOTDECK_DISABLE_LOCAL_AUTH: '0',
    SERVER_PORT: String(coexistPorts.server), VITE_PORT: String(coexistPorts.vite),
    PILOTDECK_E2E_SERVER_PORT: String(coexistPorts.server), PILOTDECK_E2E_VITE_PORT: String(coexistPorts.vite),
    PILOTDECK_E2E_GATEWAY_PORT: String(coexistPorts.gateway), PILOTDECK_GATEWAY_PORT: String(coexistPorts.gateway),
    PILOTDECK_GATEWAY_URL: `ws://127.0.0.1:${coexistPorts.gateway}/ws` });
  // The native pairing run drives the frozen baseline native-auth spec.
  if (frontend === 'new') env.REAL_NATIVE_URL = coexistOrigin;
  // The rollback switch is the only difference: unset means byte-equivalent
  // baseline catchall behaviour (old bundle keeps serving the prefix).
  if (phase === 'rollback') delete env.PILOTDECK_NEW_SHELL;
  else env.PILOTDECK_NEW_SHELL = '1';
  assertCoexistExecutionEnv(env);
  return env;
}
export function assertCoexistExecutionEnv(env) {
  assertRealExecutionEnv(env);
  const phase = env.FE_COEXIST_PHASE;
  if (env.FE_SUITE !== 'coexist' || !PHASES.includes(phase)) throw new Error('Invalid coexist phase provenance');
  if (!['old', 'new'].includes(env.FE_FRONTEND)) throw new Error('Invalid coexist frontend provenance');
  if (env.FE_FRONTEND === 'new' && env.REAL_NATIVE_URL !== coexistOrigin) throw new Error('Coexist new side must point REAL_NATIVE_URL at the coexistence origin');
  ownedPath(env.PILOTDECK_E2E_RUN_DIR); ownedPath(env.FE_OUTPUT);
  if (phase === 'rollback' ? env.PILOTDECK_NEW_SHELL !== undefined : env.PILOTDECK_NEW_SHELL !== '1') {
    throw new Error(`FAIL: coexist ${phase} new-shell switch must be ${phase === 'rollback' ? 'unset' : '1'}`);
  }
  const expected = {
    HOST: '127.0.0.1', PROXY_HOST: '127.0.0.1', PILOTDECK_GATEWAY_HOST: '127.0.0.1', SKIP_BROWSER_OPEN: '1',
    SERVER_PORT: String(coexistPorts.server), VITE_PORT: String(coexistPorts.vite),
    PILOTDECK_E2E_SERVER_PORT: String(coexistPorts.server), PILOTDECK_E2E_VITE_PORT: String(coexistPorts.vite),
    PILOTDECK_E2E_GATEWAY_PORT: String(coexistPorts.gateway), PILOTDECK_GATEWAY_PORT: String(coexistPorts.gateway),
    PILOTDECK_GATEWAY_URL: `ws://127.0.0.1:${coexistPorts.gateway}/ws`,
    PILOTDECK_E2E_LLM_CENTER_URL: `http://127.0.0.1:${realPorts.mock}`, PILOTDECK_E2E_API_KEY: REAL_TEST_KEY,
    PILOTDECK_CONFIG_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'pilotdeck.yaml'),
    DATABASE_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'auth.db'), WORKSPACES_ROOT: resolve(env.PILOTDECK_E2E_RUN_DIR, 'workspace'),
    PILOTDECK_GATEWAY_TOKEN_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'home/server-token'),
    PILOTDECK_DISABLE_LOCAL_AUTH: '0',
  };
  for (const [name, value] of Object.entries(expected)) if (env[name] !== value) throw new Error(`FAIL: coexist ${name} missing or outside the isolated deployment (${phase})`);
}

// ---- Option A patch (applied only inside the deployment copy) ----
export const optionAPatch = {
  mountAnchor: '// Serve React app for all other routes (excluding static files)',
  branchAnchor: '    // Only serve index.html for HTML routes, not for static assets',
  staticMount: `// #927 coexistence deployment (Plan 004 F3 Option A): new-shell bundle assets
// mounted at the probe path, before the SPA catchall.
app.use('${NEW_SHELL_MOUNT}', express.static(path.join(__dirname, '../new-shell'), {
    setHeaders: (res, filePath) => {
        if (filePath.endsWith('.html')) {
            res.setHeader('Cache-Control', 'no-cache, no-store, must-revalidate');
            res.setHeader('Pragma', 'no-cache');
            res.setHeader('Expires', '0');
        } else if (filePath.match(/\\.(js|css|woff2?|ttf|eot|svg|png|jpg|jpeg|gif|ico)$/)) {
            res.setHeader('Cache-Control', 'public, max-age=31536000, immutable');
        }
    }
}));`,
  prefixBranch: `    // #927 Option A: with the switch on, the migrated prefix is served by the
    // new shell instead of the SPA bundle (strict segment boundary).
    if (process.env.PILOTDECK_NEW_SHELL === '1' && (req.path === '${NEW_SHELL_PREFIX}' || req.path.startsWith('${NEW_SHELL_PREFIX}/'))) {
        res.setHeader('Cache-Control', 'no-cache, no-store, must-revalidate');
        res.setHeader('Pragma', 'no-cache');
        res.setHeader('Expires', '0');
        return res.sendFile(path.join(__dirname, '../new-shell/index.html'));
    }`,
};
export function applyOptionAPatch(source) {
  const { mountAnchor, branchAnchor } = optionAPatch;
  for (const anchor of [mountAnchor, branchAnchor]) {
    const occurrences = source.split(anchor).length - 1;
    if (occurrences !== 1) throw new Error(`A-patch anchor must occur exactly once (${occurrences}): ${anchor}`);
  }
  if (source.includes('PILOTDECK_NEW_SHELL')) throw new Error('A-patch target already carries the new-shell switch');
  return source
    .replace(mountAnchor, `${optionAPatch.staticMount}\n\n${mountAnchor}`)
    .replace(branchAnchor, `${optionAPatch.prefixBranch}\n\n${branchAnchor}`);
}
// Behavioural execution of the actual patch branch: evaluate the deployed
// template string against a fake req/res/process, the way the patched
// ui/server would. Offline oracles assert on what this returns, not on a
// hand-copied mirror of the predicate (a mirror can drift from the template).
export function executeOptionAPatchBranch(pathname, env) {
  let served = null;
  new Function('req', 'res', 'process', 'path', '__dirname', optionAPatch.prefixBranch)(
    { path: pathname },
    { setHeader() {}, sendFile(file) { served = file; } },
    { env },
    { join: (...parts) => parts.join('/') },
    '/copy/ui/server');
  return served;
}

async function buildNewShellDirectory() {
  const newShell = resolve(coexistCopyRoot, 'ui/new-shell');
  await mkdir(newShell);
  await cp(newDist, newShell, { recursive: true, filter: source => !source.endsWith('package.json') });
  // Rewrite the generated entrypoint's cross-shell asset refs to the single
  // static mount the A patch adds, and boot the boundary before the app module.
  const original = await readFile(resolve(newShell, 'index.html'), 'utf8');
  const rewritten = original
    .replaceAll('/vendor/xterm.css', `${NEW_SHELL_MOUNT}/xterm.css`)
    .replaceAll('/shared/probe.css', `${NEW_SHELL_MOUNT}/probe.css`)
    .replaceAll('/vendor/xterm.js', `${NEW_SHELL_MOUNT}/xterm.js`);
  if (rewritten === original || !rewritten.includes(NEW_SHELL_ENTRY_MARKER)) throw new Error('new-shell index.html rewrite anchors missing');
  const html = rewritten.replace('<head>',
    `<head><script type="module" src="${NEW_SHELL_MOUNT}/boundary-boot.js"></script>`);
  if (html === rewritten) throw new Error('new-shell boundary injection anchor missing');
  await writeFile(resolve(newShell, 'index.html'), html);
  await copyFile(resolve(root, 'ui-rs/shared/boundary.js'), resolve(newShell, 'boundary.js'));
  await copyFile(resolve(root, 'ui-rs/shared/probe.css'), resolve(newShell, 'probe.css'));
  await copyFile(requireUI.resolve('@xterm/xterm'), resolve(newShell, 'xterm.js'));
  await copyFile(requireUI.resolve('@xterm/xterm/css/xterm.css'), resolve(newShell, 'xterm.css'));
  await writeFile(resolve(newShell, 'boundary-boot.js'),
    `// #927: install the router/history boundary before either shell initializes.\n`
    + `import { installBoundary } from './boundary.js';\n`
    + `installBoundary(window, undefined, [${JSON.stringify(NEW_SHELL_PREFIX)}]);\n`);
}
async function injectOldShellBoundary() {
  // The boundary must run before the React router's module in the old bundle.
  const file = resolve(coexistCopyRoot, 'ui/dist/index.html');
  const html = await readFile(file, 'utf8');
  const anchor = '<script type="module" crossorigin src="/assets/';
  const occurrences = html.split(anchor).length - 1;
  if (occurrences !== 1) throw new Error(`old-shell boundary injection anchor must occur exactly once (${occurrences})`);
  await writeFile(file, html.replace(anchor,
    `<script type="module" src="${NEW_SHELL_MOUNT}/boundary-boot.js"></script>\n    ${anchor}`));
}
export async function assembleCoexistDeployment(evidenceDir) {
  for (const directory of [oldDist, newDist]) {
    if (!existsSync(resolve(directory, 'index.html'))) {
      throw new Error(`Missing trusted build output ${directory}; run build-old.mjs and build-new.py first`);
    }
  }
  await rm(coexistDeployRoot, { recursive: true, force: true });
  await mkdir(coexistDeployRoot, { recursive: true });
  // Fresh copy of the read-only baseline (its untracked dist trees are never
  // used; the old bundle comes from the trusted build-old.mjs output).
  const excludes = ['.git', '/node_modules', '/ui/node_modules', '/ui/dist', '/dist', '/work', '/evidence'];
  await run('rsync', ['-a', ...excludes.flatMap(item => ['--exclude', item]), repoBaseline + '/', coexistCopyRoot + '/']);
  await symlink(resolve(repoBaseline, 'node_modules'), resolve(coexistCopyRoot, 'node_modules'));
  await symlink(resolve(baseline, 'node_modules'), resolve(coexistCopyRoot, 'ui/node_modules'));
  await run('rsync', ['-a', oldDist + '/', resolve(coexistCopyRoot, 'ui/dist') + '/']);
  await buildNewShellDirectory();
  await injectOldShellBoundary();
  const original = await readFile(resolve(baseline, 'server/index.js'), 'utf8');
  const patched = applyOptionAPatch(original);
  await writeFile(resolve(coexistCopyRoot, 'ui/server/index.js'), patched);
  await writeFile(resolve(coexistCopyRoot, 'deployment.json'), JSON.stringify({
    source: repoBaseline, copy: coexistCopyRoot, prefix: NEW_SHELL_PREFIX, mount: NEW_SHELL_MOUNT,
    switch: 'PILOTDECK_NEW_SHELL', oldBundle: oldDist, newBundle: newDist,
  }, null, 2) + '\n');
  if (evidenceDir) {
    await copyFile(resolve(baseline, 'server/index.js'), resolve(evidenceDir, 'a-patch.original.js'));
    await copyFile(resolve(coexistCopyRoot, 'ui/server/index.js'), resolve(evidenceDir, 'a-patch.patched.js'));
  }
}
