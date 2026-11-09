import {mkdtemp,mkdir,writeFile,readFile,readdir,appendFile,rm} from "node:fs/promises";
import {tmpdir} from "node:os"; import {join} from "node:path";
import {pathToFileURL} from "node:url";
const baseline=process.env.PDRS_BASELINE; if(!baseline) throw Error("PDRS_BASELINE required");
const {createLocalGateway}=await import(pathToFileURL(join(baseline,"src/cli/createLocalGateway.ts")).href);
const root=await mkdtemp(join(tmpdir(),"pdrs-compact-oracle-"));const project=join(root,"project");await mkdir(project);
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
const app=createLocalGateway({projectRoot:project,pilotHome:project,fallbackProjectRoot:project});
try{
 const g=app.gateway;const sessionKey="compact-source";
 const archive={schemaVersion:1,format:"portable_text_messages",messages:[{role:"user",text:"old"}]};
 await g.restoreSessionTranscript({projectKey:project,sessionKey,archive});
 async function walk(path){let out=[];for(const e of await readdir(path,{withFileTypes:true})){const p=join(path,e.name);if(e.isDirectory())out.push(...await walk(p));else if(e.isFile()&&p.endsWith(".jsonl"))out.push(p);}return out;}
 const files=await walk(project);let found=[];
 for(const path of files){let rows=(await readFile(path,"utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);if(rows.some(x=>x.sessionId===sessionKey))found.push({path,rows});}
 if(found.length!==1)throw Error("expected one transcript: "+found.length);
 const {path,rows}=found[0];const last=rows.at(-1);const msg=(text)=>({role:"user",content:[{type:"text",text}]});
 const base={sessionId:sessionKey,createdAt:"2026-01-01T00:00:00.000Z"};
 const events=[{...base,entryId:"compact-entry",turnId:"t1",sequence:last.sequence+1,type:"control_boundary",boundary:{kind:"compact",subtype:"compact_boundary",snapshot:{version:1,messages:[msg("compact")]}}},{...base,entryId:"new-entry",turnId:"t2",sequence:last.sequence+2,type:"accepted_input",messages:[msg("new")]}];
 await appendFile(path,events.map(e=>JSON.stringify(e)).join("\n")+"\n");
 const value=await g.exportSessionTranscript({projectKey:project,sessionKey});
 console.log(JSON.stringify({baseline:"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",events,value},null,2));
}finally{await app.dispose();await rm(root,{recursive:true,force:true});}
