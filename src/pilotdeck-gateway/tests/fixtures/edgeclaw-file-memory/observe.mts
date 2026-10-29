import { readdirSync, readFileSync, writeFileSync, mkdirSync, chmodSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
export const NOW = '2026-10-22T00:00:00.000Z';
const RealDate: any = Date;
export function freezeClock() {
  const ms = Date.parse(NOW);
  class FrozenDate extends Date { constructor(...args: any[]) { super(...((args.length ? args : [ms]) as [number])); } static now() { return ms; } }
  (globalThis as any).Date = FrozenDate;
}
// A clock that advances 1ms per read, starting at NOW. Timestamps embedded in
// observations (exportedAt, manifest "Updated" lines, importedAt) then expose
// the exact number and order of `nowIso()` reads, so a port that reads the
// clock a different number of times drifts and fails the byte comparison.
// Install after seeding (which stays frozen) and before the observed phase.
export function stepClock() {
  const ms = Date.parse(NOW);
  let step = 0;
  class SteppingDate extends RealDate { constructor(...args: any[]) { super(...((args.length ? args : [ms + step++]) as [number])); } static now() { return ms + step++; } }
  (globalThis as any).Date = SteppingDate;
}
export function tree(root: string, dir = ''): any[] {
  return readdirSync(join(root, dir), {withFileTypes:true}).sort((a,b)=>a.name < b.name ? -1 : a.name > b.name ? 1 : 0).flatMap(e => {
    const p = dir ? `${dir}/${e.name}` : e.name;
    return e.isDirectory() ? tree(root,p) : [{relativePath:p,content:readFileSync(join(root,p),'utf8')}];
  });
}
export function clean(v: any, root: string): any {
  if (typeof v === 'string') return v.split(root).join('$ROOT');
  if (Array.isArray(v)) return v.map(x=>clean(x,root));
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,clean(x,root)]));
  return v === undefined ? null : v;
}
export function resolveRefs(v: any, vars: Record<string, any>): any {
  if (typeof v === 'string' && v.startsWith('@')) { const [key,...parts] = v.slice(1).split('.'); let x = vars[key!]; for(const p of parts) x = x[p!]; return x; }
  if (Array.isArray(v)) return v.map(x=>resolveRefs(x,vars));
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,resolveRefs(x,vars)]));
  return v;
}
export function invoke(store: any, method: string, input: any) {
  switch(method) {
    case 'getMemoryRecordsByIds': return store.getMemoryRecordsByIds(input.ids,input.maxLines);
    case 'getProjectMeta': case 'hasVisibleProjectMemory': return store[method](input.projectId);
    case 'markEntriesDeprecated': case 'restoreEntries': case 'deleteEntries': return store[method](input.relativePaths);
    case 'getOverview': case 'getSnapshotVersion': return store[method](input.lastDreamAt);
    case 'mergeDuplicateEntries': return store.mergeDuplicateEntries(input.entries ?? store.listMemoryEntries({includeDeprecated:true,limit:5000}));
    case 'toCandidate': return store.toCandidate(input.record);
    default: return store[method](input);
  }
}
export function replay(Store: any, root: string, options: any, operations: any[]) {
  const store = new Store(root,options); const vars: Record<string,any> = {}; const observations: any[] = [];
  for (const op of operations) {
    const input = resolveRefs(op.input ?? {},vars); let value: any;
    try {
      if (op.method === 'writeRaw') { const p = join(root,input.relativePath); mkdirSync(dirname(p),{recursive:true}); writeFileSync(p,input.content); value = null; }
      else if (op.method === 'chmod') { chmodSync(join(root,input.relativePath),input.mode); value = null; }
      else if (op.method === 'removeDir') { rmSync(join(root,input.relativePath),{recursive:true,force:true}); value = null; }
      else value = invoke(store,op.method,input);
      if (op.saveAs) vars[op.saveAs] = value;
      value = clean(value,root);
    } catch(e: any) { value = e.code ? {error:e.code} : {error:e.name,message:e.message}; }
    observations.push(value);
  }
  const readback = clean(readBack(store),root);
  return {observations,readback,files:tree(root)};
}
export function readBack(store: any) {
  const entries = store.listMemoryEntries({kinds:['user','project','feedback','general_project_meta'],includeDeprecated:true,limit:5000});
  return {rootDir:store.getRootDir(),workspaceMode:store.getWorkspaceMode(),generalMode:store.isGeneralMode(),profilePath:store.getUserProfileRelativePath(),entries,records:store.getMemoryRecordsByIds(entries.map((e:any)=>e.relativePath),5000),summary:store.getUserSummary(),metas:store.listProjectMetas(),hints:store.listProjectIdentityHints(),overview:store.getOverview('2026-10-20'),bundle:store.exportBundleRecords(),version:store.getSnapshotVersion('2026-10-20'),snapshot:store.exportSnapshotFiles()};
}

// #975: the direct store maps ids, then filters missing/invalid records. It does
// not apply the repository's string/blank-id filter before reading the paths.
export function maxLinesOrderOperations() {
  const op = (method: string, input: any) => ({method, input});
  const badLimit = {toString: null};
  return [
    op('writeRaw', {relativePath: 'Project/invalid.md', content: 'invalid'}),
    op('writeRaw', {relativePath: 'Project/valid.md', content: '---\nname: valid\ndescription: valid\ntype: project\nscope: project\nupdated_at: 2026-10-22T00:00:00.000Z\n---\none\ntwo\n'}),
    ...[[], ['Project/missing.md'], ['Project/invalid.md'], null, {},
      [''], ['Project/valid.md']].map(ids => op('getMemoryRecordsByIds', {ids, maxLines: badLimit})),
    // Missing ids must retain the undefined receiver's map error.
    op('getMemoryRecordsByIds', {maxLines: badLimit}),
  ];
}
