import { readFileSync, readdirSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { freezeClock, clean, tree, invoke as invokeStore } from '../edgeclaw-file-memory/observe.mts';
export {freezeClock, clean};
export const configs = [
  {id:'main',workspace:'$ROOT/home/.pilotdeck'},
  {id:'alpha',workspace:'/fixture/Alpha'},
  {id:'beta',workspace:'/fixture/Beta'},
  {id:'empty',workspace:'/fixture/Empty'},
  {id:'general',workspace:'$ROOT/home/.pilotdeck'},
  {id:'defaults',workspace:'$ROOT/workspaces',defaults:true},
  // Dedicated global store with more entries than the shared two, so dropped
  // versus forwarded limit/offset/query produce different byte output.
  {id:'probe',workspace:'/fixture/Probe',global:'$ROOT/global-probe'},
];
// JSON cannot carry NaN; the generator writes {$number:'NaN'} and both this
// replay path and the Rust harness decode it into an actual Number(NaN).
function probeNumber(v:any):number|undefined {
  if(!v || typeof v!=='object' || Array.isArray(v) || Object.keys(v).length!==1 || !('$number' in v)) return undefined;
  if(v.$number==='NaN') return NaN;
  if(v.$number==='Infinity') return Infinity;
  if(v.$number==='-Infinity') return -Infinity;
  return undefined;
}
export function expand(v:any,root:string):any {
  if(typeof v==='string') return v.replaceAll('$ROOT',root);
  if(Array.isArray(v)) return v.map(x=>expand(x,root));
  if(v && typeof v==='object') {
    const number=probeNumber(v);
    if(number!==undefined) return number;
    return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,expand(x,root)]));
  }
  return v;
}
export function openRepos(Repo:any,root:string) {
  return Object.fromEntries(configs.map(c=>[c.id,new Repo(join(root,'workspaces',c.id,'control.sqlite'),c.defaults?{}:{memoryDir:join(root,'workspaces',c.id,'memory'),globalRootDir:expand((c as any).global??'$ROOT/global',root),workspaceDir:expand(c.workspace,root)})]));
}
export function fileTree(root:string) {
  return tree(root).filter((e:any)=>!e.relativePath.endsWith('control.sqlite') && !e.relativePath.endsWith('control.sqlite-wal') && !e.relativePath.endsWith('control.sqlite-shm'));
}
export function invoke(repos:any, op:any, root:string):any {
  const r = repos[op.repo??'main']; const input = expand(op.input??{},root);
  try {
    switch(op.method) {
      case 'close': r.close(); return null;
      case 'mode': return r.getWorkspaceMode();
      case 'accessorIdentity': return [r.getFileMemoryStore()===r.getFileMemoryStore(),r.getGlobalUserStore()===r.getGlobalUserStore(),r.getFileMemoryStore()!==r.getGlobalUserStore()];
      case 'roots': return [r.getFileMemoryStore().getRootDir(),r.getGlobalUserStore().getRootDir()];
      case 'workspace': return invokeStore(r.getFileMemoryStore(),op.action,input);
      case 'global': return invokeStore(r.getGlobalUserStore(),op.action,input);
      case 'state': r.setPipelineState('workspaceDir',input); return null;
      case 'readState': return r.getPipelineState('workspaceDir')??null;
      case 'raw': {const p=join(root,input.path);mkdirSync(dirname(p),{recursive:true});writeFileSync(p,input.content); return null;}
      case 'remove': rmSync(join(root,input.path),{recursive:true,force:true});return null;
      case 'project': return r.getReadableProject(input.id)??null;
      case 'entries': return r.listReadableProjectEntries(input.id,input.options??{});
      case 'reopen': try { r.close(); } catch {}; repos[op.repo??'main'] = new r.constructor(join(root,'workspaces',op.repo??'main','control.sqlite'),{memoryDir:join(root,'workspaces',op.repo??'main','memory'),globalRootDir:expand((configs.find(c=>c.id===(op.repo??'main')) as any)?.global??'$ROOT/global',root),workspaceDir:expand(input.workspace,root)});return null;
      default: return r[op.method](input)??null;
    }
  } catch(e:any) { return {error:e.code??e.name,message:e.message}; }
}
export function observe(Repo:any,root:string,operations:any[]) {
  const repos = openRepos(Repo,root);
  const observations = operations.map(op=>clean(invoke(repos,op,root),root));
  for(const r of Object.values(repos) as any[]) { try { r.close(); } catch {} }
  return {observations,files:fileTree(root)};
}
