// Real SessionInteractionBundle owners; only HTTP transport is controlled.
import {createHash} from "node:crypto";
import {execFileSync} from "node:child_process";
import {mkdir,readFile,writeFile} from "node:fs/promises";
import assert from "node:assert/strict";
import {SessionInteractionBundle} from "/Users/da/ws/pdrs-runner/resources/PilotDeck/src/cli/SessionInteractionBundle.ts";
const fixedRoot="/Users/da/ws/pdrs-runner/resources/PilotDeck";
assert.equal(execFileSync("git",["-C",fixedRoot,"rev-parse","HEAD"],{encoding:"utf8"}).trim(),"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85");
assert.equal(process.versions.node,"22.22.0");assert.equal(process.versions.icu,"78.2");
assert.equal(createHash("sha256").update(await readFile(fixedRoot+"/src/cli/SessionInteractionBundle.ts")).digest("hex"),"e31efd0d2525410915b9ec8f8a23baac757db90455ab73a2ece8a8a5f49fd209");
const out=process.argv[2];await mkdir(out);
const rows=[];const trace=[];
const delay=(ms)=>new Promise(r=>setTimeout(r,ms));
const deadline=setTimeout(()=>{throw new Error("lifecycle capture deadline");},10000);
for(const event of ["ConfigChange","FileChanged"]){
  for(const mode of ["slow","async"]){
    const disposers=[];const registrations=[];let configHandler;let entered;let release;
    const called=new Promise(r=>entered=r);const gate=new Promise(r=>release=r);
    const networkFetch=async(url,init)=>{
      trace.push({event,mode,url:String(url),body:JSON.parse(String(init.body))});entered();
      if(mode==="slow")await gate;
      return new Response(JSON.stringify(mode==="async"?{async:true,asyncHookId:"fixture-invocation"}:{}),{status:200});
    };
    const bundle=new SessionInteractionBundle({sessionKey:"s",profile:{permissionProvider:"disabled",questionProvider:"disabled",policyMode:"interactive"},canPrompt:false,permissionRules:[],hookSettings:{},sdkHooks:{url:"http://127.0.0.1:1/hooks",events:{[event]:[{}]}},shell:{execute:async()=>{throw new Error("unexpected shell");}},projectRoot:"/fixture",networkFetch,gateway:{registerAsyncHook:r=>registrations.push(r),registerConfigChangeHandler:h=>{configHandler=h;return()=>{configHandler=undefined;};},registerSessionHookHandler:()=>()=>{}}});
    bundle.attach({own:d=>disposers.push(d)});
    let writeSettled=false;
    const write=event==="FileChanged"?bundle.fileUpdateNotifier.didSave({relativePath:"file",absolutePath:"/fixture/file",root:"/fixture",previousContent:null}).then(()=>{writeSettled=true;}):undefined;
    if(event==="ConfigChange")configHandler({changedPaths:["pilotdeck.yaml"],changeClasses:["models"]});
    await called;
    let closeSettled=false;let writeHeld;
    if(mode==="slow"){
      await delay(30);writeHeld=event==="FileChanged"?!writeSettled:undefined;
      const close=Promise.all(disposers.map(d=>d())).then(()=>{closeSettled=true;});
      await delay(30);assert.equal(closeSettled,false);if(event==="FileChanged")assert.equal(writeHeld,true);
      release();await close;await write;
      rows.push({event,mode,closeWaitsForAcceptedRequest:true,...(writeHeld===undefined?{}:{fileUpdateWaitsForHook:writeHeld}),closedAfterRelease:closeSettled});
    }else{
      await write;await delay(20);await Promise.all(disposers.map(d=>d()));
      rows.push({event,mode,registered:registrations.map(r=>({hookEvent:r.hookEvent,invocationId:r.invocationId,includeHookEvents:r.includeHookEvents}))});
    }
    await bundle.lifecycle.dispose();
  }
}
clearTimeout(deadline);
assert.equal(rows.find(r=>r.event==="ConfigChange"&&r.mode==="async").registered.length,0);
assert.equal(rows.find(r=>r.event==="FileChanged"&&r.mode==="async").registered.length,1);
await writeFile(out+"/raw.json",JSON.stringify({node:process.version,trace,rows},null,2)+"\n",{flag:"wx"});
await writeFile(out+"/lifecycle.json",JSON.stringify({baseline:"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",node:process.version,rows},null,2)+"\n",{flag:"wx"});
