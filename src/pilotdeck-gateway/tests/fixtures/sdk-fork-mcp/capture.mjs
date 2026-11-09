// Self-written fixture: execute AST-extracted fixed TS function, not a reimplementation.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFileSync,writeFileSync,mkdirSync,realpathSync} from 'node:fs';
import {join,resolve as resolvePath,dirname,basename} from 'node:path';
import {createHash} from 'node:crypto';
const args=process.argv.slice(2);
assert.ok(args.length===2||(args.length===1&&process.env.PDRS_BASELINE),'usage: capture.mjs BASELINE OUT_DIR, or PDRS_BASELINE=... capture.mjs OUT_DIR');
const base=realpathSync(resolvePath(args.length===2?args[0]:process.env.PDRS_BASELINE));
const requested=resolvePath(args.at(-1));
const out=join(realpathSync(dirname(requested)),basename(requested));
assert.ok(out!==base&&!out.startsWith(base+'/'),'output must not be inside the read-only baseline');
mkdirSync(out,{mode:0o700}); // exclusive; EEXIST is an error, never reuse evidence

const sourceCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
assert.equal(process.env.PDRS_VERIFIED_TS_SHA,sourceCommit);assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const req=createRequire(join(base,'package.json')),ts=req('typescript'),hashes={},spans={},warnings=[];
process.on('warning',warning=>warnings.push({name:warning.name,message:warning.message}));
globalThis.fetch=()=>{throw Error('fixture forbids external network')};
const expectedSourceSha256={"src/gateway/protocol/types.ts":"e3c9d4e11b3a2acbc3dd5445896bbaef1cef332a82cfe11d4da0668de4de589a","tsconfig.json":"2911e204056ccad033ca4a60d57accbb4b23b8d9872b7f952c5faeb9e894cc52","src/gateway/dialog/errors.ts":"20668c4b0074c1cd2b9d375b11a3a8c93c47faddf8264844b6a6206f1630804c","src/cli/ProjectRuntimeRegistry.ts":"0df6b0e82be872c2a2d57f85e73136d75865ff1fc2c4ded2e5e5550d7a9353d9"};
function source(file) {const text=readFileSync(join(base,file),'utf8');hashes[file]=createHash('sha256').update(text).digest('hex');assert.equal(hashes[file],expectedSourceSha256[file],file+' differs from the fixed oracle source');return text;}
source('src/gateway/protocol/types.ts');
assert.equal(ts.version,"5.9.3");
const config=JSON.parse(source('tsconfig.json'));assert.equal(config.compilerOptions.esModuleInterop,true);
const options={module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022,esModuleInterop:true};
const errorSource=source('src/gateway/dialog/errors.ts');
const errors={};new Function('exports',ts.transpileModule(errorSource,{compilerOptions:options}).outputText)(errors);
assert.equal(typeof errors.DialogGatewayError,'function');
const file='src/cli/ProjectRuntimeRegistry.ts',raw=source(file),tree=ts.createSourceFile(file,raw,ts.ScriptTarget.ES2022,true);
const functions=[],callbacks=[];
function visit(node) {
 if(ts.isFunctionDeclaration(node)&&node.name?.text==='resolveSdkSubagentMcpServers')functions.push(node);
 if(ts.isPropertyAssignment(node)&&node.name?.getText(tree)==='getSdkMcpServers'&&ts.isArrowFunction(node.initializer))callbacks.push(node.initializer);
 ts.forEachChild(node,visit);
}
visit(tree);assert.equal(functions.length,1);assert.equal(callbacks.length,1);
function span(name,node) {
 const text=node.getText(tree);const start=node.getStart(tree),end=node.end;
 spans[name]={start,end,line:tree.getLineAndCharacterOfPosition(start).line+1,sha256:createHash('sha256').update(text).digest('hex')};return text;
}
const functionText=span('resolveSdkSubagentMcpServers',functions[0]);
const functionJs=ts.transpileModule(functionText,{compilerOptions:options}).outputText;
const resolve=new Function('DialogGatewayError',functionJs+'\nreturn resolveSdkSubagentMcpServers;')(errors.DialogGatewayError);
const callbackText=span('sessionFactory.getSdkMcpServers',callbacks[0]);
const callbackJs=ts.transpileModule('const getSdkMcpServers='+callbackText+';',{compilerOptions:options}).outputText;
const callbackFactory=new Function(callbackJs+'\nreturn getSdkMcpServers;');
const C1={type:'stdio',command:'/fixture/mcp-one',args:['one'],env:{TOKEN:'fixture-only'},cwd:'/fixture/project',timeout:1000};
const C2={type:'streamable_http',url:'http://127.0.0.1:1/mcp',headers:{Authorization:'Bearer fixture-only'}};
const originalEntries=[['enabled',{config:C1,enabled:true}],['disabled',{config:C2,enabled:false}],['other',{config:C2,enabled:true}]];
const cases=[
 {id:'string-enabled',specs:['enabled']},
 {id:'string-disabled',specs:['disabled'],error:'SDK_AGENT_MCP_REFERENCE_DISABLED'},
 {id:'string-missing',specs:['missing'],error:'SDK_AGENT_MCP_REFERENCE_NOT_FOUND'},
 {id:'string-no-session-map',specs:['enabled'],noMap:true,error:'SDK_AGENT_MCP_REFERENCE_NOT_FOUND'},
 {id:'inline',specs:{local:C1}},
 {id:'inline-two-servers',specs:{alpha:C1,beta:C2}},
 {id:'duplicate-inline',specs:[{same:C1},{same:C2}],error:'INVALID_MCP_SERVER'},
 {id:'duplicate-string',specs:['enabled','enabled']},
 {id:'duplicate-string-three',specs:['enabled','other','enabled']},
 {id:'inline-then-string-same',specs:[{enabled:C2},'enabled']},
 {id:'string-then-inline-same',specs:['enabled',{enabled:C2}],error:'INVALID_MCP_SERVER'},
 {id:'mixed',specs:['enabled',{inline:C1},'other']},
 {id:'mixed-disabled',specs:[{local:C1},'disabled'],error:'SDK_AGENT_MCP_REFERENCE_DISABLED'},
 {id:'mixed-missing',specs:['enabled',{local:C1},'missing'],error:'SDK_AGENT_MCP_REFERENCE_NOT_FOUND'},
 {id:'empty-array',specs:[]},
 {id:'empty-inline',specs:{}},
];
const rows=[];
for(const entry of cases) {
 const specs=structuredClone(entry.specs);const sessionMap=entry.noMap?undefined:new Map(structuredClone(originalEntries));
 const before=JSON.stringify({specs,map:sessionMap?[...sessionMap]:null});
 let output,clones;
 try {
  const result=resolve('child',specs,sessionMap);
  assert.equal(entry.error,undefined,entry.id+' should throw');
  clones={};
  for(const [name,value] of Object.entries(result)) {
   const original=sessionMap?.get(name)?.config;
   if(original)clones[name]={object:value!==original,args:!Array.isArray(value.args)||value.args!==original.args,env:!value.env||value.env!==original.env};
  }
  output={ok:true,servers:result,keys:Object.keys(result)};
 } catch(error) {
  assert.equal(error.name,'DialogGatewayError');assert.equal(error.code,entry.error,entry.id);
  output={ok:false,error:{name:error.name,code:error.code,message:error.message,...(error.details?{details:error.details}:{})}};
 }
 assert.equal(JSON.stringify({specs,map:sessionMap?[...sessionMap]:null}),before,entry.id+' must not mutate source');
 rows.push({id:entry.id,specs:entry.specs,sessionServers:entry.noMap?null:originalEntries,output,...(clones?{clones}:{})});
}
const sessionMap=new Map(structuredClone(originalEntries)),get=callbackFactory.call({sdkSessionMcpServers:new Map([['parent',sessionMap]])});
const visible=get('parent'),missing=get('missing');
assert.deepEqual(Object.keys(visible),['enabled','other']);assert.deepEqual(missing,{});
assert.equal(visible.enabled,sessionMap.get('enabled').config,'actual callback forwards config identity');
await new Promise(resolve=>setImmediate(resolve));assert.deepEqual(warnings,[]);
const oracle={metadata:{sourceCommit,node:process.version,icu:process.versions.icu,typescript:ts.version,esModuleInterop:true,sourceSha256:hashes,extraction:spans,method:'TypeScript AST exact function/callback text transpiled in memory; actual DialogGatewayError class; no full registry/session construction',errorProjection:'name/code/message/details only; stack excluded before capture',normalization:'none',network:'Seatbelt deny all real network and fork; pure function, no default process/provider ports'},rows,sessionFactoryCallback:{sessionEntries:originalEntries,visible,visibleKeys:Object.keys(visible),missing,forwardsConfigIdentity:true}};
writeFileSync(join(out,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({rows:rows.length,sessionCallback:2}));
