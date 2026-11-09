import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
const [base,out]=process.argv.slice(2);
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2') throw Error('Node/ICU differs');
if(execFileSync('git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85') throw Error('baseline SHA differs');
const source=await readFile(join(base,'src/web/server/readSessionMessages.ts'));
if(createHash('sha256').update(source).digest('hex')!=='04a3e179c08b1e1400916de032c559a4022df4023c6254349ed545bfa32e3eea') throw Error('baseline source differs');

globalThis.fetch=()=>{throw Error('network disabled')};
const {GatewaySessionHistoryBundle}=await import(pathToFileURL(join(base,'src/cli/GatewaySessionHistoryBundle.ts')));
const {readTranscript}=await import(pathToFileURL(join(base,'src/session/transcript/TranscriptReader.ts')));
const stamp='2026-01-02T03:04:05.678Z';
const row=(type,sequence,extra={})=>({type,sessionId:'p',turnId:'t',sequence,createdAt:stamp,entryId:`e${sequence}`,...extra});
const start=(id,path)=>row('subagent_started',1,{subagentId:id,subagentType:'explore',promptPreview:'inspect',promptTruncated:false,transcriptRelativePath:path,subagentSessionId:'sub-session'});
const message=(seq,text,metadata)=>row('assistant_message',seq,{message:{role:'assistant',content:[{type:'text',text}],...(metadata?{metadata}:{})}});
const compact=(seq)=>row('control_boundary',seq,{boundary:{kind:'compact',subtype:'compact_boundary',compactMetadata:{trigger:'manual',preTokens:10,postTokens:4,messagesSummarized:2}}});
const baseChild=[row('accepted_input',1,{messages:[{role:'user',content:[{type:'text',text:'FORK-PRELUDE'}]}]}),compact(2),message(3,'CHILD'),compact(4),message(5,'SECRET',{synthetic:true}),message(6,'HIDDEN',{compactReplacement:true})];
const cases=[
{id:'missing-reference',parent:[],child:baseChild},
{id:'unknown-id',parent:[start('other','sub/first.jsonl')],child:baseChild},
{id:'child',parent:[start('child','sub/first.jsonl')],child:baseChild},
{id:'first-duplicate',parent:[start('child','sub/first.jsonl'),{...start('child','sub/second.jsonl'),sequence:2,entryId:'other'}],child:baseChild},
{id:'missing-child',parent:[start('child','sub/missing.jsonl')],child:baseChild},
{id:'invalid-child-path',parent:[start('child','../outside.jsonl')],child:baseChild},
{id:'background',background:true,parent:[start('child','child.jsonl')],child:baseChild},
{id:'background-invalid',background:true,path:'../bad.jsonl',parent:[start('child','child.jsonl')],child:baseChild},
{id:'parent-override',parentOverride:true,parent:[start('child','sub/first.jsonl')],child:baseChild},
{id:'tool-thinking',parent:[start('child','sub/first.jsonl')],child:[row('assistant_message',1,{message:{role:'assistant',content:[{type:'thinking',thinking:'reason'},{type:'tool_call',id:'c',name:'read_file',input:{path:'abc'}}]}}),row('tool_result_message',2,{message:{role:'user',content:[{type:'tool_result',toolCallId:'c',isError:false,content:[{type:'text',text:'done'}],raw:{toolName:'read_file'}}]}})]},
];
const results=[];
for(const spec of cases){
 const dir=join(out,spec.id),chat=join(dir,'chat'),home=join(dir,'home'),project=join(dir,'project');await mkdir(join(chat,'sub'),{recursive:true});await mkdir(join(chat,'tasks'),{recursive:true});await mkdir(home);await mkdir(project);
 const parentPath=spec.background?join(chat,'tasks','parent.jsonl'):join(chat,'parent.jsonl');
 const childPath=spec.background?join(chat,'tasks','child.jsonl'):join(chat,'sub','first.jsonl');
 const lines=x=>x.map(e=>JSON.stringify(e)).join('\n')+(x.length?'\n':'');
 await writeFile(parentPath,lines(spec.parent));await writeFile(childPath,lines(spec.child));await writeFile(join(chat,'sub','second.jsonl'),lines([message(1,'WRONG')]));
 const calls=[];const storage={chatDir:chat,transcriptPath:parentPath,readTranscript:async()=>{calls.push({op:'parent',path:parentPath});return readTranscript(parentPath)},readTranscriptAtPath:async path=>{calls.push({op:'child',path});return readTranscript(path)}};
 const input={sessionKey:'p',subagentId:'child',...(spec.parentOverride?{parentSessionId:'parent-override'}:{}),...(spec.background?{sessionKind:'background_task',relativeTranscriptPath:spec.path??'tasks/parent.jsonl'}:{})};
 const options={fallbackProjectRoot:project,pilotHome:home,sessionCatalog:{list:async()=>[]},now:()=>new Date(stamp),transactionOwner:{},resolveStorage:arg=>{calls.push({op:'resolveStorage',sessionId:arg.sessionId});return storage}};let result,error;
 const bundle=new GatewaySessionHistoryBundle(options);
 try{result=await bundle.readSubagentMessages(input)}catch(e){error={name:e.name,message:e.message}}
 const p=await readTranscript(parentPath),c=await readTranscript(childPath);
 const record={id:spec.id,input,options:{projectRoot:'<CASE>/project',pilotHome:'<CASE>/home',now:stamp},parent:p.entries,child:c.entries,calls,result,error};
 const normalized=JSON.parse(JSON.stringify(record).split(dir).join('<CASE>'));results.push(normalized);await writeFile(join(dir,'result.raw.json'),JSON.stringify(record,null,2)+'\n');
 console.log(JSON.stringify({id:spec.id,total:result?.total,error,calls}));
}
await writeFile(join(out,'oracle.json'),JSON.stringify({cases:results},null,2)+'\n');
