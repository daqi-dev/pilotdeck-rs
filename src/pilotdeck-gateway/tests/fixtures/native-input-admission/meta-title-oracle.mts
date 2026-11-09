import {execFileSync} from 'node:child_process';
import {readFileSync,writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const root=process.env.PDRS_BASELINE!;if(!root||!process.env.PDRS_ORACLE_OUT)throw Error('paths required');
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
function verifySource() {
for(const rel of ['src/agent/turn/TurnRunner.ts','src/agent/turn/TurnInputProcessor.ts','src/context/input/InputProcessor.ts'])if(!execFileSync('git',['-C',root,'show','cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85:'+rel]).equals(readFileSync(root+'/'+rel)))throw Error('source mismatch '+rel);
}
verifySource();
const imp=async(rel:string)=>import(pathToFileURL(root+'/'+rel).href);
const {TurnRunner}=await imp('src/agent/turn/TurnRunner.ts');const {TurnInputProcessor}=await imp('src/agent/turn/TurnInputProcessor.ts');const {InputProcessor}=await imp('src/context/input/InputProcessor.ts');
const calls:string[]=[];const writer={async recordAcceptedInput(){calls.push('accepted')},snapshotState(){return {sequence:1}},async recordAgentStatusMessage(){calls.push('status')}};
const metadata={getSnapshot(){return {}},async record(){calls.push('metadata')},async reappendTail(){calls.push('reappend')},async saveAiTitle(){calls.push('save-title')}};
const eventRecorder={async startTurn(){calls.push('start')},async completeTurn(){calls.push('complete')}};
const title={async generate(){calls.push('title-model');return null}};
const loop={async *run(){calls.push('main-model');},snapshotFileState(){return {}}};
const runner=new TurnRunner(loop,writer,new TurnInputProcessor(new InputProcessor()),()=>new Date('2026-01-02T03:04:05.678Z'),undefined,{cwd:'/fixture',transcriptPath:'',collectFileArtifacts:false},{eventRecorder,metadataStore:metadata,sessionTitleProvider:title,autoGenerateSessionTitle:true});
const events=[];for await(const event of runner.run({sessionId:'s',turnId:'t',input:{type:'text',text:'meta text',isMeta:true},messages:[],abortSignal:new AbortController().signal}))events.push(event);
verifySource();
writeFileSync(process.env.PDRS_ORACLE_OUT!,JSON.stringify({calls,events},null,2),{flag:'wx'});
