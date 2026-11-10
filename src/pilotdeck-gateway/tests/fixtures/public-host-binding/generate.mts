/** Real fixed TS project selection + original public HTTP error mapping. */
import {mkdirSync,writeFileSync,existsSync,unlinkSync} from 'node:fs';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
const baseline=resolve(process.env.PDRS_BASELINE!),out=resolve(process.argv[2]);
const baselineCommit=assertBaseline(baseline);
const {projectForStaffDeckPrincipal,projectBindingEnvironment}=await import(pathToFileURL(join(baseline,'src/pilot/staffdeck/projectBinding.ts')).href);
const {handlePublicHostHttpRequest}=await import(pathToFileURL(join(baseline,'src/composition/publicHostHttpTransport.ts')).href);
const {ProjectRuntimeRegistry}=await import(pathToFileURL(join(baseline,'src/cli/ProjectRuntimeRegistry.ts')).href);
const home=join(out,'home'),project=join(out,'project');mkdirSync(join(home,'staffdeck'),{recursive:true});mkdirSync(project,{recursive:true});
const file=join(home,'staffdeck/project-bindings.json');
const principal={pilotDeckUserId:'owner',tenantId:'tenant',actorUserId:'actor',agentId:'target'};
const env={PILOT_HOME:home,STAFFDECK_PROJECT_BINDING_ENABLED:'true',STAFFDECK_COPY_TENANT_ID:'tenant',STAFFDECK_COPY_ACTOR_USER_ID:'actor',STAFFDECK_COPY_PILOTDECK_USER_ID:'owner',STAFFDECK_FORMAL_API_ORIGIN:'http://127.0.0.1:9/'};
const bound={projectRoot:project,bindingId:'fixture-binding',agentName:'fixture',origin:'http://127.0.0.1:9',tenantId:'tenant',agentId:'target',status:'bound',updatedAt:'2026-11-11T00:00:00.000Z'};
const noOrigin:any={...bound};delete noOrigin.origin;
const store=(projects:any)=>({version:1,installationId:'fixture-install',projects});
const cases:any[]=[
 {case:'disabled-no-store',enabled:false,principal,store:undefined},
 {case:'bound',principal,store:store([bound])},
 {case:'binding-no-lock',principal,store:store([{...bound,status:'binding',runtimeReady:true}])},
 {case:'binding-live-lock',principal,store:store([{...bound,status:'binding',runtimeReady:true}]),liveLock:true},
 {case:'duplicate-bound',principal,store:store([bound,{...bound,bindingId:'duplicate'}])},
 {case:'actor-mismatch',principal:{...principal,actorUserId:'other'},store:store([bound])},
 {case:'tenant-mismatch',principal:{...principal,tenantId:'other'},store:store([bound])},
 {case:'owner-mismatch',principal:{...principal,pilotDeckUserId:'other'},store:store([bound])},
 {case:'agent-mismatch',principal:{...principal,agentId:'other'},store:store([bound])},
 {case:'origin-mismatch',principal,store:store([{...bound,origin:'http://127.0.0.1:8'}])},
 {case:'missing-store',principal,store:undefined},
 {case:'null-project-row',principal,store:store([null])},
 {case:'null-store',principal,store:null},
 {case:'number-store',principal,store:3},
 {case:'string-store',principal,store:'not-an-object'},
 {case:'array-store',principal,store:[]},
 {case:'empty-project-root',principal,store:store([{...bound,projectRoot:''}])},
 {case:'origin-both-absent',principal,store:store([noOrigin]),omitOriginEnvironment:true},
 {case:'number-project-root',principal,store:store([{...bound,projectRoot:3}]),registryBoundary:true},
 {case:'boolean-project-root',principal,store:store([{...bound,projectRoot:true}]),registryBoundary:true},
 {case:'array-project-root',principal,store:store([{...bound,projectRoot:[]}]),registryBoundary:true},
];
const registry=new ProjectRuntimeRegistry({fallbackProjectRoot:project,pilotHome:home,env:{PILOT_HOME:home},permissionMode:'default',permissionTimeoutMs:5000,elicitationTimeoutMs:5000,now:()=>new Date(),buildBrowserUseArgs:(args:any)=>args} as any);
let selected:any;
const provider={operations:['list_model_catalog'],async call(){return{status:200,body:{unused:true}}}};
// This minimal resolver preserves the verified createLocalGateway.ts guard;
// it does not instantiate a project runtime or claim full production boot.
const server=createServer((req,res)=>{void handlePublicHostHttpRequest(req,res,{token:'binding-fixture-token',resolve:(caller:any)=>{
 const root=projectForStaffDeckPrincipal(caller,selected.env);
 if(selected.env.STAFFDECK_PROJECT_BINDING_ENABLED==='true'&&!root)throw Error('PROJECT_BINDING_PRINCIPAL_REQUIRED');
 if(selected.row.registryBoundary)registry.resolve(root as any);
 return provider;
}}).then(handled=>{if(!handled){res.writeHead(404);res.end()}}).catch(error=>res.destroy(error));});
server.listen(0,'127.0.0.1');await once(server,'listening');const address=server.address()as{port:number};
const errorValue=(error:any)=>({name:error.name,code:error.code??null,status:error.status??null,message:error.message});
const raw:any[]=[];
try{
 for(const row of cases){
  if(existsSync(file))unlinkSync(file);if(existsSync(file+'.lock'))unlinkSync(file+'.lock');
  if(row.store!==undefined)writeFileSync(file,JSON.stringify(row.store));if(row.liveLock)writeFileSync(file+'.lock',String(process.pid),{flag:'wx'});
  selected={row,env:{...env,STAFFDECK_PROJECT_BINDING_ENABLED:row.enabled===false?'false':'true'}};if(row.omitOriginEnvironment)delete selected.env.STAFFDECK_FORMAL_API_ORIGIN;
  let selection:any;try{const root=projectForStaffDeckPrincipal(row.principal,selected.env);selection={ok:true,rootType:typeof root,root:root??null}}catch(error){selection={ok:false,error:errorValue(error)}}
  let runtimeTarget:any;try{runtimeTarget=projectBindingEnvironment(project,selected.env).STAFFDECK_COPY_TARGET_AGENT_ID??null}catch(error){runtimeTarget={error:errorValue(error)}}
  const response=await fetch(`http://127.0.0.1:${address.port}/api/module-host/describe`,{method:'POST',headers:{authorization:'Bearer binding-fixture-token','content-type':'application/json'},body:JSON.stringify({principal:row.principal})});
  const responseText=await response.text();raw.push({case:row.case,liveLockOwnerPid:row.liveLock?process.pid:null,status:response.status,body:responseText});
  row.selection=selection;row.runtimeTarget=runtimeTarget;row.response={status:response.status,body:JSON.parse(responseText)};
 }
}finally{server.closeAllConnections();server.close();await once(server,'close');await registry.disposeProjectRuntimes();await registry.disposeSessionMcpRuntimes();if(existsSync(file+'.lock'))unlinkSync(file+'.lock')}
const sourceSha256=await baselineSourceHashes(baseline,['src/pilot/staffdeck/projectBinding.ts','src/cli/createLocalGateway.ts','src/composition/publicHostHttpTransport.ts']);
const normalize=(v:any):any=>typeof v==='string'?v.replaceAll(home,'<HOME>').replaceAll(project,'<PROJECT>'):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,x])=>[k,normalize(x)])):v;
const nodeVersions=Object.fromEntries(['node','icu','uv'].map(k=>[k,process.versions[k]]));if(JSON.stringify(nodeVersions)!==JSON.stringify({node:'22.22.0',icu:'78.2',uv:'1.52.1'}))throw Error('fixed Node required');
writeFileSync(join(out,'raw-capture.json'),JSON.stringify(raw,null,2)+'\n',{flag:'wx'});
writeFileSync(join(out,'oracle.json'),JSON.stringify({baselineCommit,sourceSha256,nodeVersions,scope:'ORIGINAL_BINDING_AND_ORIGINAL_HTTP_TRANSPORT_WITH_MINIMAL_RESOLVER_NOT_FULL_GATEWAY_BOOT',environment:normalize(env),cases:normalize(cases)},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:cases.length,out}));
