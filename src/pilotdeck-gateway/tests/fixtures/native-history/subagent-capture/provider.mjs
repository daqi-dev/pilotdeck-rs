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
globalThis.fetch=()=>{throw Error('offline')};
const load=async p=>import(pathToFileURL(join(base,p)));
const {GatewaySessionHistoryBundle}=await load('src/cli/GatewaySessionHistoryBundle.ts');
const {JsonlSessionPersistence}=await load('src/session/persistence/JsonlSessionPersistence.ts');
const {getPilotProjectChatDir}=await load('src/pilot/paths.ts');
const date='2026-01-02T03:04:05.678Z';
const row=(type,sequence,extra={})=>({type,sessionId:'parent',turnId:'t',sequence,createdAt:date,entryId:`e${sequence}`,...extra});
const start=(extra={})=>row('subagent_started',1,{subagentId:'child',subagentType:'explore',promptPreview:'inspect',promptTruncated:false,transcriptRelativePath:'../decoy.jsonl',subagentSessionId:'sub-session',...extra});
const answer=row('assistant_message',1,{message:{role:'assistant',content:[{type:'text',text:'PROVIDER CHILD'}]}});
const cases=[{id:'provider',parent:[start()]},{id:'provider-missing-session',parent:[start({subagentSessionId:undefined})]},{id:'provider-duplicate',parent:[start(),{...start({subagentSessionId:'other'}),sequence:2}]},{id:'provider-parent-override',parent:[start()],parentOverride:true},{id:'background-path',background:true,parent:[start({transcriptRelativePath:'child.jsonl'})]},{id:'background-invalid',background:true,parent:[start()],path:'../outside.jsonl'}];
const results=[];
for(const spec of cases){
 const dir=join(out,spec.id),home=join(dir,'home'),project=join(dir,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});const chat=getPilotProjectChatDir(project,home);
 const parent=spec.parentOverride?'other-parent':'parent';await mkdir(join(chat,parent,'subagents'),{recursive:true});await mkdir(join(chat,'tasks'),{recursive:true});
 const lines=v=>v.map(e=>JSON.stringify(e)).join('\n')+'\n';
 await writeFile(join(chat,`${parent}.jsonl`),lines(spec.parent));await writeFile(join(chat,parent,'subagents','child.jsonl'),lines([answer]));
 await writeFile(join(chat,'tasks','parent.jsonl'),lines(spec.parent));await writeFile(join(chat,'tasks','child.jsonl'),lines([answer]));
 const calls=[];const provider={create(input){calls.push({kind:input.kind,sessionId:input.sessionId,parentSessionId:input.parentSessionId,sidechainId:input.sidechainId,transcriptPath:input.transcriptPath});return {persistence:new JsonlSessionPersistence({path:input.transcriptPath}),projectionCheckpointStore:{load(){throw Error('unexpected checkpoint')}}};}};
 const input={sessionKey:'parent',subagentId:'child',...(spec.parentOverride?{parentSessionId:'other-parent'}:{}),...(spec.background?{sessionKind:'background_task',relativeTranscriptPath:spec.path??'tasks/parent.jsonl'}:{})};
 const options={fallbackProjectRoot:project,pilotHome:home,sessionCatalog:{list:async()=>[]},now:()=>new Date(date),transactionOwner:{},storageProvider:provider};let result,error;
 try{result=await new GatewaySessionHistoryBundle(options).readSubagentMessages(input)}catch(e){error={name:e.name,message:e.message}}
 const record={id:spec.id,input,calls,result,error};await writeFile(join(dir,'result.raw.json'),JSON.stringify(record,null,2)+'\n');results.push(JSON.parse(JSON.stringify(record).split(dir).join('<CASE>')));console.log(JSON.stringify(record));
}
await writeFile(join(out,'oracle.json'),JSON.stringify({cases:results},null,2)+'\n');
