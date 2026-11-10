import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,mkdtempSync,mkdirSync,rmSync} from 'node:fs';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baseArg,outArg]=process.argv.slice(2);assert(baseArg&&outArg);
const baseline=resolve(baseArg),out=resolve(outArg);
assert.equal(process.versions.node,'22.22.0');assert.equal(process.versions.icu,'78.2');
const commit=assertBaseline(baseline);
const sourceSha256=await baselineSourceHashes(baseline,['src/tool/builtin/agent.ts','src/agent/sub/builtinSubagentTypes.ts','src/tool/builtin/subagentContinuation.ts','src/tool/builtin/executeCode.ts']);
const {createAgentTool}=await import(pathToFileURL(join(baseline,'src/tool/builtin/agent.ts')).href);
const {createSubagentContinuationTool,createSendMessageTool}=await import(pathToFileURL(join(baseline,'src/tool/builtin/subagentContinuation.ts')).href);
const {createExecuteCodeTool,setExecuteCodeTransportOverrideForTests}=await import(pathToFileURL(join(baseline,'src/tool/builtin/executeCode.ts')).href);
async function capture(tool:any){
 const metadata:any={};
 for(const key of ['aliases','kind','requiredRuntimeCapabilities','maxResultBytes'])if(tool[key]!==undefined)metadata[key]=tool[key];
 return {model:{name:tool.name,description:tool.description,inputSchema:tool.inputSchema},metadata,flags:{
   isReadOnly:await tool.isReadOnly({}),isConcurrencySafe:await tool.isConcurrencySafe({}),
   isOpenWorld:tool.isOpenWorld?await tool.isOpenWorld({}):undefined},
   rawSerializableDefinition:JSON.parse(JSON.stringify(tool))};
}
const custom={fixture:{type:'general-purpose',description:'CUSTOM PRESET DESCRIPTION',systemPrompt:'CUSTOM PRESET SYSTEM'}};
const customRust={fixture:{allowedTools:['read_file'],disallowedTools:['bash'],systemPromptSuffix:'CUSTOM PRESET SYSTEM'}};
const matrix:any[]=[
 {id:'default',options:{},rustConfig:{}},
 {id:'custom-only',options:{subagents:custom},rustConfig:{subagentDefinitions:customRust}},
 {id:'empty-custom',options:{subagents:{}},rustConfig:{subagentDefinitions:{}}},
 {id:'open',options:{maxSubagentDepth:2,subagentDepth:0},rustConfig:{maxSubagentDepth:2,subagentDepth:0}},
 {id:'custom-open',options:{subagents:custom,maxSubagentDepth:2,subagentDepth:0},rustConfig:{subagentDefinitions:customRust,maxSubagentDepth:2,subagentDepth:0}},
 ...[
  ['last-level',2,1],['at-cap',2,2],['zero-cap',0,0],['negative-depth-clamped',2,-2],
  ['fractional-closed',1.5,0.5],['fractional-open',1.6,0.5],['deeper-open',4,2],
 ].map(([id,maxSubagentDepth,subagentDepth])=>({id,options:{maxSubagentDepth,subagentDepth},rustConfig:{maxSubagentDepth,subagentDepth}})),
];
const agent=[];
for(const row of matrix)agent.push({...row,expected:await capture(createAgentTool(row.options))});
assert.equal(agent[0].expected.model.description,agent[1].expected.model.description);
assert.equal(agent[0].expected.model.description,agent[2].expected.model.description);
assert.equal(agent[3].expected.model.description,agent[4].expected.model.description);
assert(!agent[1].expected.model.description.includes('CUSTOM PRESET DESCRIPTION'));
const port={start:async()=>{throw Error('constructor must not start subagent')},followup:async()=>{throw Error('constructor must not send message')}};
const continuation=[
 {id:'subagent',followup:false,expected:await capture(createSubagentContinuationTool(port))},
 {id:'send-message',followup:true,expected:await capture(createSendMessageTool(port))},
];
const neverPrepare={prepare:async()=>{throw Error('description-only construct must not prepare')}};
const executeDescriptions=[];
for(const webSearch of [false,true])for(const mode of [null,'danger-full-access','read-only','workspace-write',undefined]){
 const options=mode===null?{webSearch}:{webSearch,sandbox:{mode,port:neverPrepare,resolvePolicy:()=>{throw Error('not executing')}}};
 executeDescriptions.push({
  id:mode===null?'no-sandbox':mode===undefined?'mode-omitted':mode,
  ...(mode===undefined?{tsOnly:true}:{mode}),
  webSearch,description:createExecuteCodeTool(options).description,
 });
}
const scratch=mkdtempSync(join(tmpdir(),'pdrs-policy-'));const policySelections=[];
try {
 mkdirSync(join(scratch,'workspace'));
 setExecuteCodeTransportOverrideForTests('tcp');
 for(const selectedMode of ['read-only','workspace-write']){
  let prepared:any;let ran=0;
  const runtime={enforcesSandbox:false,resolveExecutable:async()=>'/fixture/mock-python',run:async()=>{ran++;throw Error('policy capture must stop before code run')}};
  const workspace={create:async()=>{
   const root=mkdtempSync(join(scratch,'execution-'));
   return {root,writeText:async(name:string,text:string)=>{const file=join(root,name);writeFileSync(file,text,{flag:'wx'});return file;},cleanup:async()=>rmSync(root,{recursive:true,force:true})};
  }};
  const tool=createExecuteCodeTool({sandbox:{mode:'workspace-write',resolvePolicy:({workspaceRoot,executionRoot}:any)=>({mode:selectedMode,workspaceRoot,executionRoot,network:'deny'}),port:{prepare:async(request:any)=>{prepared={mode:request.policy.mode,network:request.policy.network};throw Error('policy-probe-stop');}}},codeRuntime:runtime,executionWorkspace:workspace});
  const result=await tool.execute({code:"print('policy')",timeout_seconds:1,max_tool_calls:0},{cwd:join(scratch,'workspace'),env:{PATH:process.env.PATH,LANG:'C'},executeTool:async()=>{throw Error('no helper calls expected')}});
  assert.equal(prepared.mode,selectedMode);assert.equal(result.data.status,'error');assert.equal(result.data.error,'policy-probe-stop');assert.equal(ran,0);
  assert(tool.description.includes('host workspace-write file-effect policy'));
  policySelections.push({configuredMode:'workspace-write',resolvedAndPreparedPolicy:prepared,codeRuntimeRunCount:ran,resultStatus:result.data.status,resultError:result.data.error,description:tool.description});
 }
} finally {setExecuteCodeTransportOverrideForTests(undefined);rmSync(scratch,{recursive:true,force:true});}
const oracle={schema:1,sourceCommit:commit,node:process.versions.node,icu:process.versions.icu,sourceSha256,agent,continuation,executeDescriptions,policySelections,scope:'Actual TS constructors and selected policy path. Policy provider intentionally rejects before any Python execution; not a filesystem-enforcement claim.'};
writeFileSync(out,JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({agentCases:agent.length,continuationCases:continuation.length,policyCases:policySelections.length,sha256:createHash('sha256').update(readFileSync(out)).digest('hex')}));
