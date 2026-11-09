import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const root=process.env.PDRS_BASELINE ?? process.cwd();
const files=['src/composition/domainPorts.ts','src/composition/HttpModuleClient.ts'];
const git=(...args)=>execFileSync('git',['-C',root,...args],{encoding:'utf8'}).trim();
assert.equal(git('rev-parse','HEAD'),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const blobs=Object.fromEntries(files.map(path=>[path,git('rev-parse',`HEAD:${path}`)]));
for(const path of files) assert.equal(git('hash-object',path),blobs[path]);
const {createKnowledgeModulePort,createKnowledgeQueryTool}=await import(pathToFileURL(`${root}/${files[0]}`).href);
const cases=[
 {id:'camel',input:{query:'q',modelConfigId:'camel-choice'},credential:false},
 {id:'snake',input:{query:'q',model_config_id:'snake-choice'},credential:false},
 {id:'both',input:{query:'q',modelConfigId:'A',model_config_id:'B',tenantId:'forged'},credential:false},
 {id:'credential-camel',input:{query:'q',modelConfigId:'A'},credential:true},
 {id:'credential-snake',input:{query:'q',model_config_id:'B'},credential:true},
];
const output=[];
for(const c of cases){
 const wire=[];
 globalThis.fetch=async(url,options={})=>{
  const body=options.body?JSON.parse(String(options.body)):null;
  wire.push({path:new URL(String(url)).pathname,method:options.method??'GET',body});
  if(!body)return Response.json({protocolVersion:'2.0',implementationId:'staffdeck.knowledge',contract:'staffdeck.knowledge/v1',transport:'module-http-v2',methods:['query']});
  return Response.json({kind:'response',messageId:'reply',inReplyTo:body.messageId,requestId:body.requestId,ok:true,payload:{result:{answer:'fixture'}}});
 };
 const binding={enabled:true,implementationId:'staffdeck.knowledge',contract:'staffdeck.knowledge/v1',transport:'module-http-v2',endpoint:'https://fixture.invalid',methods:['query'],tenantId:'trusted-tenant',actorUserId:'trusted-user',agentId:'trusted-agent',...(c.credential?{credentialEnv:'FAKE_ONLY'}:{})};
 const tool=createKnowledgeQueryTool(createKnowledgeModulePort(binding),binding);
 let result,error;
 try {result=await tool.execute(c.input);} catch(e){error={name:e.name,code:e.code,message:e.message};}
 output.push({id:c.id,input:c.input,wire,...(result===undefined?{}:{result}),...(error===undefined?{}:{error})});
}
assert.equal(output.filter(x=>x.wire.some(r=>r.method==='POST')).length,3);
assert.equal(output.filter(x=>x.error?.code==='MODULE_PROTOCOL_INCOMPATIBLE').length,2);
for(const path of files)assert.equal(git('hash-object',path),blobs[path]);
await writeFile(process.argv[2],JSON.stringify({baseline:git('rev-parse','HEAD'),blobs,node:process.version,icu:process.versions.icu,cases:output},null,2)+'\n',{flag:'wx'});
