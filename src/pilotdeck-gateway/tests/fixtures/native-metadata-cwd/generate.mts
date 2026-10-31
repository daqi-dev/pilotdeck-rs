import assert from 'node:assert/strict';
import {mkdir, mkdtemp, realpath, rm, writeFile, symlink} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1154 pinned Node');
assert.ok(process.env.PDRS_BASELINE, '#1154 explicit read-only baseline');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sources = ['src/pilot/paths.ts', 'src/web/server/listProjects.ts',
  'src/gateway/dialog/projectRegistry.ts', 'src/cli/GatewaySessionModelBundle.ts',
  'src/cli/ProjectRuntimeRegistry.ts', 'src/always-on/runtime/SessionConfigOverrides.ts',
  'src/gateway/client/InProcessGateway.ts'];
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const {createProjectId, resolveProjectStorageId} = await load(sources[0]);
const {createRegisteredWebProjectResolver, listRegisteredWebProjects, listWebProjects, describeWebProject} = await load(sources[1]);
const {createDialogProjectRegistry} = await load(sources[2]);
const {GatewaySessionModelBundle} = await load(sources[3]);
const {ProjectRuntimeRegistry} = await load(sources[4]);
const {SessionConfigOverrides} = await load(sources[5]);
const {InProcessGateway} = await load(sources[6]);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
const root = await realpath(await mkdtemp(join(output, 'work-')));
const old = process.cwd();
const cases = [];
let sessionCwd;
try {
  for (const [name, marker, cwd] of [
    ['relative-project', '.', 'project'], ['relative-other', '.', 'other'],
    ['absolute-other', '{project}', 'other'], ['absolute-deleted', '{project}', 'deleted'],
    ['relative-deleted', '.', 'deleted'],
    ['symlink-scan', '{project}', 'other'], ['collision-deleted', '{project}', 'deleted'],
    ['collision-other', '{project}', 'other'],
  ]) {
    const home = join(root, name, 'home'), project = join(root, name, 'project');
    const other = join(root, name, 'other'), deleted = join(root, name, 'deleted');
    const directory = name === 'symlink-scan' ? join(home, 'link-storage') : join(home, 'projects', name.startsWith('collision-') ? 'zzz-collision' : createProjectId(project));
    for (const dir of [directory, project, other, deleted]) await mkdir(dir, {recursive: true});
    if (name === 'symlink-scan') {
      await mkdir(join(home, 'projects'), {recursive: true});
      await symlink(directory, join(home, 'projects', 'legacy'));
    }
    await writeFile(join(directory, '.cwd'), marker.replace('{project}', project), {flag: 'wx'});
    if (name === 'absolute-deleted' || name === 'collision-deleted') {
      const unrelated = join(home, 'projects', '000-unrelated');
      await mkdir(unrelated, {recursive: true});
      await writeFile(join(unrelated, '.cwd'), '.', {flag: 'wx'});
    }
    await writeFile(join(project, 'visible.txt'), 'fixture', {flag: 'wx'});
    if (name === 'collision-other') {
      const unrelated = join(home, 'projects', '000-unrelated');
      await mkdir(unrelated, {recursive: true});
      await writeFile(join(unrelated, '.cwd'), other, {flag: 'wx'});
    }
    const resolver = createRegisteredWebProjectResolver({pilotHome: home});
    const registry = createDialogProjectRegistry({pilotHome: home, resolveProject: resolver,
      listProjects: () => listRegisteredWebProjects({pilotHome: home})});
    const bundle = new GatewaySessionModelBundle({fallbackProjectKey: project,
      resolveProjectKey: registry.resolveProjectKey,
      router: {hasActiveTurn: () => false, close: async () => {}},
      selectionPort: {read: async () => undefined},
      policy: {resolveDefault: () => ({provider: 'fixture', model: 'mock', source: 'default'})}});
    // Real gateway methods with the real listProjects port; no runtime/provider
    // construction is needed for these two disk-only calls.
    const gateway = Object.create(InProcessGateway.prototype);
    gateway.options = {listProjects: () => listWebProjects({pilotHome: home})};
    process.chdir(cwd === 'project' ? project : cwd === 'other' ? other : deleted);
    if (cwd === 'deleted') await rm(deleted, {recursive: true});
    let cwdReadable = true;
    try {process.cwd();} catch {cwdReadable = false;}
    const observations: Record<string, unknown> = {};
    for (const [method, call] of [
      ['sessionModelGet', () => bundle.sessionModelGet({projectKey: project, sessionKey: 's'})],
      ['resolveTurnModelSelection', () => bundle.resolveTurnModelSelection({projectKey: project, sessionKey: 's'})],
    ] as const) {
      try {
        const value = await call();
        observations[method] = {ok: true, source: method === 'sessionModelGet' ? value.effective.source : value.source};
      } catch (error: any) {
        observations[method] = {ok: false, code: error.code,
          message: error.message.replaceAll(root, '{root}')};
      }
    }
    const describeHasStorage = (await describeWebProject(project, {pilotHome: home})).createdAt !== undefined;
    for (const [method, call] of [
      ['projectFilesList', () => gateway.projectFilesList({projectKey: project, includeDirs: false})],
      ['listProjects', () => gateway.listProjects()],
    ] as const) {
      try {
        const value = await call();
        observations[method] = method === 'listProjects'
          ? {ok: true, count: value.projects.length}
          : {ok: true, count: value.items.length, paths: value.items.map((item: any) => item.relativePath)};
      } catch (error: any) {
        observations[method] = {ok: false, code: error.code,
          message: error.message.replaceAll(root, '{root}')};
      }
    }
    const chatUsesDirectId = resolveProjectStorageId(project, home) === createProjectId(project);
    cases.push({name, marker, cwd, cwdReadable, describeHasStorage, chatUsesDirectId, ...observations});
    process.chdir(root);
  }
  const overrides = new SessionConfigOverrides();
  const runtime = new ProjectRuntimeRegistry({fallbackProjectRoot: join(root, 'project'),
    pilotHome: join(root, 'home'), env: {}, now: () => new Date(0), sessionOverrides: overrides,
    sessionCatalog: {}, telemetry: {}, continuations: {}, buildBrowserUseArgs: (x: any) => x,
    permissionMode: 'default', permissionTimeoutMs: 1000, elicitationTimeoutMs: 1000} as any);
  runtime.setSessionCwd('s', '.');
  sessionCwd = overrides.get('s')?.cwd;
} finally {
  process.chdir(old);
  await rm(root, {recursive: true, force: true});
}
const record = {sourceCommit, sourceSha256, node: process.version, cases, sessionCwd};
await writeFile(join(output, 'oracle.json'), JSON.stringify(record, null, 2) + '\n', {flag: 'wx'});
console.log(JSON.stringify(record, null, 2));
