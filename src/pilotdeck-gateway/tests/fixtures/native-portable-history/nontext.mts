import {mkdtemp,mkdir,writeFile,readFile,readdir,appendFile,rm} from "node:fs/promises";
import {tmpdir} from "node:os";import {join} from "node:path";import {pathToFileURL} from "node:url";import {createHash} from "node:crypto";
const baseline=process.env.PDRS_BASELINE;if(!baseline)throw Error("PDRS_BASELINE required");
const {createLocalGateway}=await import(pathToFileURL(join(baseline,"src/cli/createLocalGateway.ts")).href);
const root=await mkdtemp(join(tmpdir(),"pdrs-portable-nontext-"));const project=join(root,"project");await mkdir(project);
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
const sourcePath=join(baseline,"src/cli/createLocalGateway.ts");
async function walk(path){let out=[];for(const e of await readdir(path,{withFileTypes:true})){const p=join(path,e.name);if(e.isDirectory())out.push(...await walk(p));else if(e.isFile()&&p.endsWith(".jsonl"))out.push(p);}return out;}
const variants=[
 {name:"tool_calls",messages:[{role:"assistant",content:[{type:"text",text:""},{type:"text",text:"head"},{type:"tool_call",id:"a",name:"object",input:{z:1,"10":"ten","2":"two",nested:[true,null,"汉"]}},{type:"tool_call",id:"b",name:"literal",input:"  literal  "},{type:"tool_call",id:"c",name:"nil",input:null},{type:"tool_call",id:"d",name:"absent"},{type:"tool_call",id:"e",name:"array",input:[1,"x"]}]}]},
 {name:"tool_results",messages:[{role:"user",content:[{type:"tool_result",toolCallId:"empty",content:[]},{type:"tool_result",toolCallId:"mixed",content:[{type:"text",text:""},{type:"text",text:"body"},{type:"image",source:"url",mimeType:"image/png",data:"https://example.invalid/a.png"},{type:"image",source:"base64",mimeType:"image/jpeg",data:"😀x"},{type:"pdf",source:"base64",mimeType:"application/pdf",data:"AAAA",bytes:3,pages:3},{type:"pdf",source:"base64",mimeType:"application/pdf",data:"AAAA",bytes:3,pages:0},{type:"pdf",source:"base64",mimeType:"application/pdf",data:"AAAA",bytes:3,pages:-2}]}]}]},
 {name:"references_and_outer_media",messages:[{role:"assistant",content:[{type:"tool_result_reference",toolCallId:"ref-1",path:"/owned/mock/result",originalBytes:8,preview:"ignored",hasMore:true},{type:"image",source:"url",mimeType:"image/png",data:"https://example.invalid/a.png"},{type:"pdf",source:"base64",mimeType:"application/pdf",data:"AAAA",bytes:3,pages:1},{type:"audio",source:"base64",mimeType:"audio/wav",data:"AAAA"},{type:"thinking",text:"not portable"}]}]},
 {name:"synthetic_and_empty",messages:[{role:"user",content:[{type:"text",text:"   "}]},{role:"assistant",content:[{type:"thinking",text:"hidden"}]},{role:"user",content:[{type:"text",text:"skip"}],metadata:{synthetic:true}},{role:"user",content:[{type:"text",text:"keep"}],metadata:{synthetic:false}}]},
];
const projections=[];const validation=[];const MAX=2*1024*1024;
const baseArchive=()=>({schemaVersion:1,format:"portable_text_messages",messages:[{role:"user",text:""}]});
const recipes=[{name:"empty_title",title:""},{name:"null_title",title:null},{name:"title_512_units",title:"😀".repeat(256)},{name:"title_513_units",title:"😀".repeat(256)+"x"},{name:"title_before_role",title:null,badRole:true},{name:"count_before_title",title:null,count:0},{name:"count_10000",count:10000},{name:"count_10001",count:10001},{name:"ascii_exact_bytes",bytes:MAX,fill:"x"},{name:"ascii_over_bytes",bytes:MAX+1,fill:"x"},{name:"cjk_exact_bytes",bytes:MAX,fill:"汉"},{name:"cjk_over_bytes",bytes:MAX+1,fill:"汉"}];
try{
 for(const variant of variants){const sessionKey="nontext:"+variant.name;await app.gateway.restoreSessionTranscript({projectKey:project,sessionKey,archive:{schemaVersion:1,format:"portable_text_messages",messages:[{role:"user",text:"seed"}]}});let found=[];for(const path of await walk(project)){const rows=(await readFile(path,"utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);if(rows.some(x=>x.sessionId===sessionKey))found.push({path,rows});}if(found.length!==1)throw Error("transcript count "+found.length);const{path,rows}=found[0];const last=rows.at(-1);const entry={sessionId:sessionKey,createdAt:"2026-01-01T00:00:00.000Z",entryId:"nontext-entry",turnId:"nontext-turn",sequence:last.sequence+1,type:"accepted_input",messages:variant.messages};await appendFile(path,JSON.stringify(entry)+"\n");try{const value=await app.gateway.exportSessionTranscript({projectKey:project,sessionKey});projections.push({name:variant.name,entries:[...rows,entry],ok:true,value});}catch(e){projections.push({name:variant.name,entries:[...rows,entry],ok:false,code:e?.code??null,message:String(e?.message??e)});}}
 for(const recipe of recipes){const archive=baseArchive();if("title"in recipe)archive.title=recipe.title;if(recipe.count!==undefined)archive.messages=Array.from({length:recipe.count},()=>({role:"user",text:"x"}));if(recipe.badRole)archive.messages[0].role="system";if(recipe.bytes){const overhead=Buffer.byteLength(JSON.stringify(archive),"utf8");const n=recipe.bytes-overhead;const width=Buffer.byteLength(recipe.fill,"utf8");archive.messages[0].text=recipe.fill.repeat(Math.floor(n/width))+"x".repeat(n%width);}const bytes=Buffer.byteLength(JSON.stringify(archive),"utf8");try{const value=await app.gateway.restoreSessionTranscript({projectKey:project,sessionKey:"validate:"+recipe.name,archive});validation.push({recipe,bytes,ok:true,value});}catch(e){validation.push({recipe,bytes,ok:false,code:e?.code??null,message:String(e?.message??e)});}}
 console.log(JSON.stringify({baseline:"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",environment:process.versions,sourceSha256:createHash("sha256").update(await readFile(sourcePath)).digest("hex"),projections,validation},null,2));
}finally{await app.dispose();await rm(root,{recursive:true,force:true});}
