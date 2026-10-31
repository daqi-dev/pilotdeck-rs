import assert from 'node:assert/strict';
import {lstat, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, stat, symlink, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1137 pinned Node');
assert.ok(process.env.PDRS_BASELINE, '#1137 explicit read-only baseline');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sources = ['src/pilot/paths.ts', 'src/web/server/listProjects.ts',
  'src/gateway/dialog/projectRegistry.ts', 'src/gateway/client/GatewayTurnReplacementCoordinator.ts'];
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const {createProjectId, resolveProjectStorageId} = await load(sources[0]);
const {createRegisteredWebProjectResolver, listRegisteredWebProjects} = await load(sources[1]);
const {createDialogProjectRegistry} = await load(sources[2]);
const {GatewayTurnReplacementCoordinator} = await load(sources[3]);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
// process.cwd() resolves macOS /var -> /private/var. Use the same spelling
// for requested projects so cwd=project remains true under the test tmpdir.
const work = await realpath(await mkdtemp(join(output, 'work-')));
const cases = [];
const originalCwd = process.cwd();
type MarkerCase = {name: string; marker?: string; markerHex?: string;
  kind?: 'file' | 'directory' | 'symlink'; cwd?: 'project' | 'other'};
const inputs: MarkerCase[] = [
  {name: 'empty', marker: '', cwd: 'project'},
  {name: 'whitespace', marker: ' \t\u00a0\ufeff ', cwd: 'project'},
  {name: 'newline', marker: '\n', cwd: 'project'}, {name: 'matching', marker: '{project}'},
  {name: 'padded', marker: ' \t{project}\n'}, {name: 'foreign', marker: '{other}'},
  {name: 'relative-dot', marker: '.'}, {name: 'relative-dot-slash', marker: './'},
  {name: 'trailing-slash', marker: '{project}/'},
  {name: 'crlf', marker: '{project}\r\n'},
  {name: 'directory', kind: 'directory'}, {name: 'dangling-symlink', kind: 'symlink'},
  {name: 'invalid-utf8', markerHex: 'ff'},
  {name: 'relative-dot-at-project', marker: '.', cwd: 'project'},
  {name: 'relative-dot-slash-at-project', marker: './', cwd: 'project'},
];
try {
  for (const {name, marker, markerHex, kind = 'file', cwd = 'other'} of inputs) {
    const home = join(work, name, 'home'), project = join(work, name, 'project');
    const other = join(work, name, 'other');
    await mkdir(project, {recursive: true});
    await mkdir(other, {recursive: true});
    const directory = join(home, 'projects', createProjectId(project));
    await mkdir(directory, {recursive: true});
    const path = join(directory, '.cwd');
    if (kind === 'directory') await mkdir(path);
    else if (kind === 'symlink') await symlink('missing-target', path);
    else {
      const bytes = markerHex === undefined
        ? Buffer.from(marker!.replace('{project}', project).replace('{other}', other))
        : Buffer.from(markerHex, 'hex');
      await writeFile(path, bytes, {flag: 'wx'});
    }
    const snapshot = async () => {
      const entry = await lstat(path);
      if (entry.isSymbolicLink()) return {kind: 'symlink', target: await readlink(path),
        targetExists: await stat(path).then(() => true, () => false)};
      if (entry.isDirectory()) return {kind: 'directory', entries: (await readdir(path)).sort()};
      assert.ok(entry.isFile(), '#1137 marker is a regular file');
      const bytesHex = (await readFile(path)).toString('hex')
        .replaceAll(Buffer.from(project).toString('hex'), Buffer.from('{project}').toString('hex'))
        .replaceAll(Buffer.from(other).toString('hex'), Buffer.from('{other}').toString('hex'));
      return {kind: 'file', bytesHex};
    };
    const before = await snapshot();
    const rawBefore = kind === 'file' ? await readFile(path) : null;
    process.chdir(cwd === 'project' ? project : other);
    const resolver = createRegisteredWebProjectResolver({pilotHome: home});
    const registry = createDialogProjectRegistry({pilotHome: home, resolveProject: resolver,
      listProjects: () => listRegisteredWebProjects({pilotHome: home})});
    const resolved = await resolver(project);
    let errorCode: string | null = null;
    try { await registry.resolveProjectKey(project); }
    catch (error: any) { errorCode = error.code; }
    const after = await snapshot();
    const rawUnchanged = rawBefore === null || rawBefore.equals(await readFile(path));
    cases.push({name, marker, markerHex, kind, cwd, registered: resolved === project, errorCode,
      before, after, markerUnchanged: rawUnchanged && JSON.stringify(before) === JSON.stringify(after)});
  }
} finally {
  process.chdir(originalCwd);
  await rm(work, {recursive: true, force: true});
}

const paths = await (async () => {
const root = await realpath(await mkdtemp(join(output, 'probe-')));
const old = process.cwd();
const result: any = {options: [], boundaries: []};
const normalized = (text: string) => text.replaceAll(root, '{root}');
try {
  const home = join(root,'home'), project = join(root,'project'), other = join(root,'other');
  for (const dir of [home, project, other, join(project,'child')]) await mkdir(dir, {recursive:true});
  await symlink(project, join(root,'alias'));
  process.chdir(root);
  for (const input of ['project', './project/', 'project/child/..', 'alias']) {
    const identity = resolve(input);
    const directory = join(home, 'projects', createProjectId(identity));
    await mkdir(directory, {recursive:true});
    // Native provisioning has no TS counterpart; seed exactly its normalized bytes.
    if (input === 'project' || input === 'alias') await writeFile(join(directory,'.cwd'), identity, {flag:'wx'});
    const resolver = createRegisteredWebProjectResolver({pilotHome:home});
    result.options.push({input, resolved:normalized(identity), registered:await resolver(identity) === identity});
  }
  const resolver = createRegisteredWebProjectResolver({pilotHome:home});
  process.chdir(other);
  for (const input of ['.', './', '', '../project', project]) {
    result.boundaries.push({input:normalized(input), resolved:normalized(resolve(input)),
      registered:await resolver(input) === project,
      storageMatches:resolveProjectStorageId(input, home) === resolveProjectStorageId(project,home)});
  }
  const doomed = join(root,'deleted');
  await mkdir(doomed);
  process.chdir(doomed);
  await rm(doomed, {recursive:true});
  try {process.cwd(); result.cwdUnreadable=false;} catch {result.cwdUnreadable=true;}
  result.deletedCwdAbsolute = await resolver(project) === project;
} finally { process.chdir(old); await rm(root,{recursive:true,force:true}); }
return result;
})();

// Rust Clone models another handle to the same TS object, not a TS clone API.
const original = new GatewayTurnReplacementCoordinator({timeoutMs: 1000, session: {
  activeTurnRunId() { return undefined; }, hasActiveTurn() { return false; },
  async abortExpectedTurn() {}, async closeSession() {},
}});
const alias = original;
original.reserveTranscriptWrite('clone-probe', 'probe');
const reservedThroughAlias = alias.hasTranscriptWriteReservation('clone-probe');
let conflictCode: string | null = null;
try { alias.reserveTranscriptWrite('clone-probe', 'probe'); }
catch (error: any) { conflictCode = error.code; }
alias.releaseTranscriptWrite('clone-probe');
const releasedThroughOriginal = !original.hasTranscriptWriteReservation('clone-probe');
const observations = {cases, paths, alias: {reservedThroughAlias, conflictCode, releasedThroughOriginal}};
const record = {sourceCommit, sourceSha256, node: process.version, observations};
await writeFile(join(output, 'oracle.json'), JSON.stringify(record, null, 2) + '\n', {flag: 'wx'});
console.log(JSON.stringify(record, null, 2));
