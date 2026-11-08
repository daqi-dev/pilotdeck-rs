import {readFileSync,writeFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
import {resolve} from 'node:path';
const root=process.env.PDRS_BASELINE;
if(!root)throw Error('PDRS_BASELINE required');
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const paths=['src/gateway/client/InProcessGateway.ts','src/cli/ProjectRuntimeRegistry.ts'];
function verify(){for(const path of paths)if(!execFileSync('git',['-C',root,'show',commit+':'+path]).equals(readFileSync(root+'/'+path)))throw Error('baseline '+path);}
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
verify();
const {InProcessGateway}=await import(pathToFileURL(root+'/'+paths[0]).href);
const {ProjectRuntimeRegistry}=await import(pathToFileURL(root+'/'+paths[1]).href);
const gateway=new InProcessGateway({} as any,{} as any);
const fallback='/private/tmp/pdrs-r96-scope-oracle-20261109-01/default';
const project='/private/tmp/pdrs-r96-scope-oracle-20261109-01/foreign';
const calls:any[]=[];
const runtime={pluginRuntime:{refresh:async()=>{calls.push('refresh');},snapshot:()=>[]}};
const registry:any=Object.create(ProjectRuntimeRegistry.prototype);
Object.assign(registry,{disposed:false,options:{fallbackProjectRoot:fallback},runtimes:new Map([[resolve(fallback),runtime],[resolve(project),runtime]]),sdkSessionPlugins:new Map(),sdkSessionMcpServers:new Map()});
const cases:any[]=[];
for(const [name,key] of [['null',null],['number',5],['foreign',project]]) {
 for(const kind of ['always','sop','resume','mcp']){
  calls.length=0;
  try {
   const value=kind==='always'?await gateway.alwaysOnApply({projectKey:key} as any):kind==='sop'?await gateway.sopStatus({sessionKey:'s',projectKey:key} as any):kind==='resume'?await gateway.resumeSop({sessionKey:'s',projectKey:key} as any):await registry.mcpServerStatusForSdk({sessionKey:'s',projectKey:key});
   cases.push({id:name+'-'+kind,ok:true,value,calls:[...calls]});
  } catch(e:any){cases.push({id:name+'-'+kind,ok:false,name:e.name,code:e.code??null,message:e.message,calls:[...calls]});}
 }
}
verify();
if(!process.env.PDRS_ORACLE_OUT)throw Error('PDRS_ORACLE_OUT required');
writeFileSync(process.env.PDRS_ORACLE_OUT,JSON.stringify({node:process.version,icu:process.versions.icu,commit,cases},null,2),{flag:'wx'});
