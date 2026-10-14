// B registrations stay frozen. Cells express deployment prerequisites, not new specs.
import { resolve } from 'node:path';
import { baseline, ownedPath, root, realPorts, REAL_TEST_KEY } from './env.mjs';
import { realExecutionEnv, assertRealExecutionEnv } from './execution-env.mjs';

export const deployPorts = Object.freeze({ server: 28410, vite: 28411, gateway: 28412, knowledge: 28413 });
export const deployOrigin = `http://127.0.0.1:${deployPorts.vite}`;
export const deployRunRoot = resolve(root, '_tmp/fe-parity/deploy-run');
export const deployAllowedPorts = Object.freeze([realPorts.mock, ...Object.values(deployPorts)]);
export const profileSources = Object.freeze({ staffdeck: 'formal-host.yaml', minimal: 'formal-minimal.yaml',
  replacement: 'replacement-knowledge.yaml', native: 'formal-native.yaml' });
export const staffdeckBlock = 'BLOCKED: 缺锁定 StaffDeck SOP/Knowledge checkout、已安装 Python/uvicorn 依赖及隔离启动/种子编排；需 kb_preset_data_001、kb_preset_sales_001、kchunk_preset_sales_001 和 operator_approval/operator_approval_alternate。mock-llm 只能替代模型，不能伪造真实模块。交人提供并确认源码/运行时后另行接入，禁止沿用用户已监听服务。';
export const formalProjects = Object.freeze([
  Object.freeze({ name: 'desktop', use: { viewport: { width: 1440, height: 1000 } } }),
  Object.freeze({ name: 'mobile', use: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true } }),
]);
export const DEPLOY_CELLS = Object.freeze([
  { id: 'staffdeck-formal', profile: 'staffdeck', kind: 'formal', files: ['formal-composition.spec.mjs'], activeLines: [13, 32, 52], blocked: staffdeckBlock },
  { id: 'replacement-formal', profile: 'replacement', kind: 'formal', files: ['formal-composition.spec.mjs'], activeLines: [59] },
  { id: 'staffdeck-auth', profile: 'staffdeck', kind: 'auth', files: ['real-auth-composition.spec.mjs'], activeLines: [52], blocked: staffdeckBlock },
  { id: 'minimal-auth', profile: 'minimal', kind: 'auth', files: ['real-auth-composition.spec.mjs'], activeLines: [134] },
  { id: 'replacement-auth', profile: 'replacement', kind: 'auth', files: ['real-auth-composition.spec.mjs'], activeLines: [150] },
  // The native Settings case needs a native profile; it cannot run on minimal.
  { id: 'native-auth', profile: 'native', kind: 'native', files: ['real-auth-native.spec.mjs'], activeLines: [6] },
  { id: 'native-routes', profile: 'native', kind: 'routes', files: ['formal-route-matrix.spec.mjs'], activeLines: [7] },
  { id: 'minimal-routes', profile: 'minimal', kind: 'routes', files: ['formal-route-matrix.spec.mjs'], activeLines: [7] },
  { id: 'replacement-routes', profile: 'replacement', kind: 'routes', files: ['formal-route-matrix.spec.mjs'], activeLines: [7] },
].map(cell => Object.freeze({ ...cell, files: Object.freeze(cell.files), activeLines: Object.freeze(cell.activeLines),
  projects: Object.freeze(['formal', 'routes'].includes(cell.kind) ? formalProjects.map(project => project.name) : ['']) })));
export function deployCell(id) {
  const cell = DEPLOY_CELLS.find(cell => cell.id === id);
  if (!cell) throw new Error(`Unknown B deployment cell: ${id}`);
  return cell;
}
export function selectDeployCells(profiles) {
  if (!profiles.length || new Set(profiles).size !== profiles.length || profiles.some(p => !['staffdeck', 'minimal', 'replacement'].includes(p))) throw new Error('Invalid B profiles');
  return DEPLOY_CELLS.filter(cell => cell.profile === 'native' || profiles.includes(cell.profile));
}
export function cellInventory(inventory, cell) {
  return inventory.filter(row => cell.files.includes(row.file)).flatMap(row => cell.projects.map(project => ({ ...(row.file === 'formal-route-matrix.spec.mjs'
    ? { ...row, title: `formal ${cell.profile} routes follow the generated composition`, canonicalTitle: row.title }
    : { ...row, canonicalTitle: row.title }), project })));
}
export const deployInstanceId = row => JSON.stringify([row.file, row.title, row.project]);
export function activeRegistration(row, cell) { return cell.activeLines.includes(row.line); }
export function profileSource(cell) {
  return resolve(baseline, '../products/pilotdeck-staffdeck-sop/profiles', profileSources[cell.profile]);
}
export function deployExecutionEnv(parent, { output, runDir, cell }) {
  // Reuse D's sanitization and OS isolation. Override every deployment endpoint.
  const env = realExecutionEnv(parent, { output, runDir, mode: 'ready' });
  Object.assign(env, { FE_SUITE: 'blackbox-deploy', FE_B_CELL: cell.id, FE_PROFILE: cell.profile,
    SKIP_BROWSER_OPEN: '1', PILOTDECK_FRONTEND_PROFILE: env.PILOTDECK_CONFIG_PATH,
    PILOTDECK_DISABLE_LOCAL_AUTH: cell.kind === 'formal' ? '1' : '0',
    SERVER_PORT: String(deployPorts.server), VITE_PORT: String(deployPorts.vite),
    PILOTDECK_E2E_SERVER_PORT: String(deployPorts.server), PILOTDECK_E2E_VITE_PORT: String(deployPorts.vite),
    PILOTDECK_E2E_GATEWAY_PORT: String(deployPorts.gateway), PILOTDECK_GATEWAY_PORT: String(deployPorts.gateway),
    PILOTDECK_GATEWAY_URL: `ws://127.0.0.1:${deployPorts.gateway}/ws`,
    REPLACEMENT_KNOWLEDGE_PORT: String(deployPorts.knowledge),
    REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH: resolve(runDir, 'replacement-calls.jsonl'),
  });
  if (cell.kind === 'formal') Object.assign(env, { FORMAL_COMPOSITION_URL: deployOrigin,
    FORMAL_REPLACEMENT: cell.profile === 'replacement' ? '1' : '0' });
  if (cell.kind === 'auth') Object.assign(env, { REAL_COMPOSITION_URL: deployOrigin, REAL_COMPOSITION_PROFILE: cell.profile });
  if (cell.kind === 'routes') Object.assign(env, { FORMAL_COMPOSITION_URL: deployOrigin,
    FORMAL_ROUTE_PROFILE: cell.profile, FORMAL_ROUTE_AUTH: '1' });
  if (cell.kind === 'native') env.REAL_NATIVE_URL = deployOrigin;
  assertDeployExecutionEnv(env);
  return env;
}
export function assertDeployExecutionEnv(env) {
  assertRealExecutionEnv(env);
  const cell = deployCell(env.FE_B_CELL);
  if (env.FE_SUITE !== 'blackbox-deploy' || env.FE_FRONTEND !== 'old' || env.FE_PROFILE !== cell.profile) throw new Error('Invalid B cell provenance');
  ownedPath(env.PILOTDECK_E2E_RUN_DIR); ownedPath(env.FE_OUTPUT);
  const expected = {
    HOST: '127.0.0.1', PROXY_HOST: '127.0.0.1', PILOTDECK_GATEWAY_HOST: '127.0.0.1', SKIP_BROWSER_OPEN: '1',
    SERVER_PORT: String(deployPorts.server), VITE_PORT: String(deployPorts.vite),
    PILOTDECK_E2E_SERVER_PORT: String(deployPorts.server), PILOTDECK_E2E_VITE_PORT: String(deployPorts.vite),
    PILOTDECK_E2E_GATEWAY_PORT: String(deployPorts.gateway), PILOTDECK_GATEWAY_PORT: String(deployPorts.gateway),
    PILOTDECK_GATEWAY_URL: `ws://127.0.0.1:${deployPorts.gateway}/ws`,
    PILOTDECK_E2E_LLM_CENTER_URL: `http://127.0.0.1:${realPorts.mock}`, PILOTDECK_E2E_API_KEY: REAL_TEST_KEY,
    PILOTDECK_CONFIG_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'pilotdeck.yaml'),
    PILOTDECK_FRONTEND_PROFILE: resolve(env.PILOTDECK_E2E_RUN_DIR, 'pilotdeck.yaml'),
    DATABASE_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'auth.db'), WORKSPACES_ROOT: resolve(env.PILOTDECK_E2E_RUN_DIR, 'workspace'),
    PILOTDECK_GATEWAY_TOKEN_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'home/server-token'),
    PILOTDECK_DISABLE_LOCAL_AUTH: cell.kind === 'formal' ? '1' : '0',
    REPLACEMENT_KNOWLEDGE_PORT: String(deployPorts.knowledge),
    REPLACEMENT_KNOWLEDGE_EVIDENCE_PATH: resolve(env.PILOTDECK_E2E_RUN_DIR, 'replacement-calls.jsonl'),
    FORMAL_COMPOSITION_URL: ['formal', 'routes'].includes(cell.kind) ? deployOrigin : undefined,
    FORMAL_REPLACEMENT: cell.kind === 'formal' ? cell.profile === 'replacement' ? '1' : '0' : undefined,
    FORMAL_ROUTE_PROFILE: cell.kind === 'routes' ? cell.profile : undefined,
    FORMAL_ROUTE_AUTH: cell.kind === 'routes' ? '1' : undefined,
    REAL_COMPOSITION_URL: cell.kind === 'auth' ? deployOrigin : undefined,
    REAL_COMPOSITION_PROFILE: cell.kind === 'auth' ? cell.profile : undefined,
    REAL_NATIVE_URL: cell.kind === 'native' ? deployOrigin : undefined,
  };
  for (const [name, value] of Object.entries(expected)) if (env[name] !== value) throw new Error(`FAIL: B ${name} missing or outside repository deployment (${cell.id})`);
}
