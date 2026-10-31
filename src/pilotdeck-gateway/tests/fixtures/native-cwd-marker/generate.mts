import assert from 'node:assert/strict';
import {mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
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
const {createProjectId} = await load(sources[0]);
const {createRegisteredWebProjectResolver, listRegisteredWebProjects} = await load(sources[1]);
const {createDialogProjectRegistry} = await load(sources[2]);
const {GatewayTurnReplacementCoordinator} = await load(sources[3]);
const output = resolve(process.argv[2]);
await mkdir(output, {recursive: true});
const work = await mkdtemp(join(output, 'work-'));
const cases = [];
try {
  for (const [name, marker] of [
    ['empty', ''], ['whitespace', ' \t\u00a0\ufeff '], ['newline', '\n'],
    ['matching', '{project}'], ['padded', ' \t{project}\n'], ['foreign', '{other}'],
  ]) {
    const home = join(work, name, 'home'), project = join(work, name, 'project');
    const other = join(work, name, 'other');
    await mkdir(project, {recursive: true});
    await mkdir(other, {recursive: true});
    const directory = join(home, 'projects', createProjectId(project));
    await mkdir(directory, {recursive: true});
    const path = join(directory, '.cwd');
    const bytes = marker.replace('{project}', project).replace('{other}', other);
    await writeFile(path, bytes, {flag: 'wx'});
    const resolver = createRegisteredWebProjectResolver({pilotHome: home});
    const registry = createDialogProjectRegistry({pilotHome: home, resolveProject: resolver,
      listProjects: () => listRegisteredWebProjects({pilotHome: home})});
    const resolved = await resolver(project);
    let errorCode: string | null = null;
    try { await registry.resolveProjectKey(project); }
    catch (error: any) { errorCode = error.code; }
    cases.push({name, marker, registered: resolved === project, errorCode,
      markerUnchanged: await readFile(path, 'utf8') === bytes});
  }
} finally { await rm(work, {recursive: true, force: true}); }

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
const observations = {cases, alias: {reservedThroughAlias, conflictCode, releasedThroughOriginal}};
const record = {sourceCommit, sourceSha256, node: process.version, observations};
await writeFile(join(output, 'oracle.json'), JSON.stringify(record, null, 2) + '\n', {flag: 'wx'});
console.log(JSON.stringify(record, null, 2));
