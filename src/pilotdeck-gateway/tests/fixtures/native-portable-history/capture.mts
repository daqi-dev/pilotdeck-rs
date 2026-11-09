import {mkdtemp,mkdir,writeFile,readFile,rm} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join} from "node:path";
import {pathToFileURL} from "node:url";
const baseline=process.env.PDRS_BASELINE; if(!baseline) throw Error("PDRS_BASELINE required");
const {createLocalGateway}=await import(pathToFileURL(join(baseline,"src/cli/createLocalGateway.ts")).href);
const {DEFAULT_MODEL_CAPABILITIES}=await import(pathToFileURL(join(baseline,"src/model/protocol/capabilities.ts")).href);
const {replayTranscriptEntries}=await import(pathToFileURL(join(baseline,"src/session/transcript/TranscriptReplay.ts")).href);
const root=await mkdtemp(join(tmpdir(),"pdrs-archive-oracle-"));
const project=join(root,"project"); await mkdir(project);
await writeFile(join(project,"pilotdeck.yaml"),`schemaVersion: 1
agent:
  model: test/test
  maxContextTokens: 65536
  maxOutputTokens: 8192
router:
  enabled: false
model:
  providers:
    test:
      protocol: openai
      url: http://127.0.0.1:1
      apiKey: fixture-only
      models:
        test:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 32768
            maxOutputTokens: 8192
`);
let release, started; const blocked=new Promise(r=>release=r);const entered=new Promise(r=>started=r);
const model={
 async *stream(){started(); await blocked; yield {type:"request_started",provider:"test",model:"test"};yield {type:"message_start",role:"assistant"};yield {type:"text_delta",text:"answer"};yield {type:"message_end",finishReason:"stop"};},
 async complete(){return {role:"assistant",content:[{type:"text",text:"answer"}],finishReason:"stop"};},
 getCapabilities(){return DEFAULT_MODEL_CAPABILITIES;},getMultimodal(){return {input:["text"]};},getProviderProtocol(){return "openai";},getProviderBaseUrl(){return undefined;}
};
const app=createLocalGateway({projectRoot:project,pilotHome:project,fallbackProjectRoot:project,__testModelFactory:()=>model});
const archive={schemaVersion:1,format:"portable_text_messages",messages:[{role:"user",text:"hello"},{role:"assistant",text:"world"}],title:"Title"};
const cases={}; async function take(name,fn){try{cases[name]={ok:true,value:await fn()}}catch(e){cases[name]={ok:false,code:e?.code??null,message:String(e?.message??e)}}}
try{
 const g=app.gateway;
 await take("unknown_project_missing_session",()=>g.exportSessionTranscript({projectKey:join(root,"unknown")}));
 await take("invalid_project_empty_session",()=>g.exportSessionTranscript({projectKey:23,sessionKey:""}));
 await take("cold_invalid_archive",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:cold",archive:{}}));
 await take("empty_archive",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:empty",archive:{schemaVersion:1,format:"portable_text_messages",messages:[]}}));
 await take("restore",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:restored",archive}));
 await take("export_restored",()=>g.exportSessionTranscript({projectKey:project,sessionKey:"sdk:restored"}));
 await take("conflict",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:restored",archive}));
 await take("missing",()=>g.exportSessionTranscript({projectKey:project,sessionKey:"sdk:missing"}));
 const run=(async()=>{for await(const e of g.submitTurn({sessionKey:"sdk:busy",channelKey:"test",workspaceCwd:project,message:"busy"})){} })();
 await Promise.race([entered,new Promise((_,rej)=>setTimeout(()=>rej(Error("model start timed out")),15000))]);
 await take("busy_invalid_archive",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:busy",archive:{}}));
 await take("busy_export",()=>g.exportSessionTranscript({projectKey:project,sessionKey:"sdk:busy"}));
 await take("busy_restore",()=>g.restoreSessionTranscript({projectKey:project,sessionKey:"sdk:busy",archive}));
 release();await run;
 await take("finished_export",()=>g.exportSessionTranscript({projectKey:project,sessionKey:"sdk:busy"}));
 const common={sessionId:"s",createdAt:"2026-01-01T00:00:00.000Z"};
 const msg=(text)=>({role:"user",content:[{type:"text",text}]});
 const entries=[{...common,type:"accepted_input",turnId:"t0",sequence:1,messages:[msg("old")]},{...common,type:"control_boundary",turnId:"t1",sequence:2,boundary:{kind:"compact",subtype:"compact_boundary",snapshot:{version:1,messages:[msg("compact")]}}},{...common,type:"accepted_input",turnId:"t2",sequence:3,messages:[msg("new")]}];
 const replay=replayTranscriptEntries(entries);
 cases.compact_projection={ok:true,messages:replay.messages};
 console.log(JSON.stringify({baseline:"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",cases},null,2));
}finally{release();await app.dispose();await rm(root,{recursive:true,force:true});}
