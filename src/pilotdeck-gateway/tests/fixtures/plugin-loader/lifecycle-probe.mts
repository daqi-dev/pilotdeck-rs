// Real baseline behavior with deterministic Promise barriers; invoked only by
// generate.mts after its fixed-tree/source-closure guard has succeeded.
export async function lifecycleProbe(PluginRuntime:any, PluginRegistry:any, root:string) {
  const options={projectRoot:root+'/project',pilotHome:root+'/home'};
  const plugin=(name:string,dispose:()=>Promise<void>)=>({name,path:root+'/'+name,source:'builtin',dispose});
  const sequential=new PluginRegistry(); let sequentialCalls=0;
  sequential.replaceAll([plugin('old',async()=>{sequentialCalls++})]);
  await sequential.replaceAll([]).disposeRemoved(); await sequential.dispose();
  const registry=new PluginRegistry(); let sharedCalls=0;
  let entered!:()=>void, finish!:()=>void;
  const started=new Promise<void>(resolve=>entered=resolve);
  const done=new Promise<void>(resolve=>finish=resolve);
  registry.replaceAll([plugin('failing',async()=>{sharedCalls++;entered();await done;throw new Error('dispose failed')})]);
  const lease=registry.acquire(); const replacement=registry.replaceAll([]);
  const retirement=replacement.disposeRemoved().catch((e:Error)=>e.message);
  const shutdown=registry.dispose().catch((e:Error)=>e.message);
  const release=lease.release().catch((e:Error)=>e.message);
  await started; finish();
  const releaseError=await release, shutdownError=await shutdown; await retirement;
  const repeatReleaseError=await lease.release().catch((e:Error)=>e.message);
  let resolveRefresh!:()=>void;
  const runtime=new PluginRuntime(options);
  runtime.refreshPromise=new Promise<void>(resolve=>resolveRefresh=resolve);
  const disposing=runtime.dispose(); const stateWhileRefresh=runtime.lifecycleState;
  let newCalls=0;
  runtime.registry.replaceAll([plugin('new',async()=>{newCalls++})]);
  resolveRefresh(); await disposing;
  let publicationError='';
  try { runtime.registry.replaceAll([]); } catch(e) { publicationError=(e as Error).message; }
  return {sequentialCalls,sharedCalls,releaseError,repeatReleaseError,shutdownError,stateWhileRefresh,newCalls,finalCount:runtime.snapshot().length,publicationError};
}
