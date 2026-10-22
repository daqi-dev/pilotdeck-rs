// Observation harness for the export/import/overview fixture. The repo list,
// expansion, file tree and store dispatch mirror `edgeclaw-accessors`; the
// export-specific methods (bundles, overviews, list/count, traces, l0 seeding)
// dispatch on `op.method` with `@var` references resolving against saved
// observations so a TS-produced bundle can be fed straight back into import.
import { readFileSync, readdirSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { freezeClock, clean, tree, invoke as invokeStore, resolveRefs } from '../edgeclaw-file-memory/observe.mts';
export {freezeClock, clean};
export const configs = [
  {id:'main',workspace:'/fixture/Main'},
  {id:'target',workspace:'/fixture/Target'},
  {id:'beta',workspace:'/fixture/Beta'},
  {id:'empty',workspace:'/fixture/Empty'},
  {id:'general',workspace:'$ROOT/home/.pilotdeck'},
];
export function expand(v:any,root:string):any {
  if(typeof v==='string') return v.replaceAll('$ROOT',root);
  if(Array.isArray(v)) return v.map(x=>expand(x,root));
  if(v && typeof v==='object') return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,expand(x,root)]));
  return v;
}
export function openRepos(Repo:any,root:string) {
  return Object.fromEntries(configs.map(c=>[c.id,new Repo(join(root,'workspaces',c.id,'control.sqlite'),{memoryDir:join(root,'workspaces',c.id,'memory'),globalRootDir:join(root,'global'),workspaceDir:expand(c.workspace,root)})]));
}
export function fileTree(root:string) {
  return tree(root).filter((e:any)=>!e.relativePath.endsWith('control.sqlite') && !e.relativePath.endsWith('control.sqlite-wal') && !e.relativePath.endsWith('control.sqlite-shm'));
}
export function invoke(repos:any, op:any, root:string, vars:Record<string,any>):any {
  const r = repos[op.repo??'main']; const input = expand(op.input??{},root);
  switch(op.method) {
    case 'close': r.close(); return null;
    case 'mode': return r.getWorkspaceMode();
    case 'workspace': return invokeStore(r.getFileMemoryStore(),op.action,input);
    case 'global': return invokeStore(r.getGlobalUserStore(),op.action,input);
    case 'state': r.setPipelineState(input.key,input.value); return null;
    case 'readState': return r.getPipelineState(input.key)??null;
    case 'settings': return r.saveIndexingSettings(input.partial,input.defaults);
    case 'export': return r.exportMemoryBundle();
    case 'import': return r.importMemoryBundle(resolveRefs(input.bundle,vars));
    case 'overview': return r.getOverview();
    case 'uiSnapshot': return 'limit' in input ? r.getUiSnapshot(input.limit) : r.getUiSnapshot();
    case 'list': return r.listMemoryEntries(input);
    case 'count': return r.countMemoryEntries(input);
    case 'l0': r.insertL0Session(input); return null;
    case 'case': r.saveCaseTrace(input.record,input.limit); return null;
    case 'index': r.saveIndexTrace(input.record,input.limit); return null;
    case 'dream': r.saveDreamTrace(input.record,input.limit); return null;
    case 'raw': {const p=join(root,input.path);mkdirSync(dirname(p),{recursive:true});writeFileSync(p,input.content); return null;}
    case 'remove': rmSync(join(root,input.path),{recursive:true,force:true});return null;
    case 'reopen': try { r.close(); } catch {}; repos[op.repo??'main'] = new r.constructor(join(root,'workspaces',op.repo??'main','control.sqlite'),{memoryDir:join(root,'workspaces',op.repo??'main','memory'),globalRootDir:join(root,'global'),workspaceDir:expand(input.workspace,root)});return null;
    default: return r[op.method](input)??null;
  }
}
export function observe(Repo:any,root:string,operations:any[]) {
  const repos = openRepos(Repo,root);
  const vars: Record<string,any> = {};
  const observations = operations.map(op=>{
    let value: any;
    try {
      value = invoke(repos,op,root,vars);
      if (op.saveAs) vars[op.saveAs] = value;
    } catch(e:any) { value = {error:e.code??e.name,message:e.message}; }
    return clean(value,root);
  });
  for(const r of Object.values(repos) as any[]) { try { r.close(); } catch {} }
  return {observations,files:fileTree(root)};
}
