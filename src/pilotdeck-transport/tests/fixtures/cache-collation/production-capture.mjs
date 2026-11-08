import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {createRequire} from 'node:module';
const [baseline,out]=process.argv.slice(2);
if(!baseline||!out)throw new Error('Usage: production-capture.mjs BASELINE NEW_OUTPUT_DIRECTORY');
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw new Error('Fixed Node/ICU required');
if(process.env.LANG!=='en_US.UTF-8'||process.env.LC_ALL!==undefined||process.env.LC_MESSAGES!==undefined)throw new Error('Use LANG=en_US.UTF-8 without LC_ALL/LC_MESSAGES');
const require=createRequire(path.join(baseline,'package.json'));
const ts=require('typescript');if(ts.version!=='5.9.3')throw new Error('Fixed TypeScript required');
const source=fs.readFileSync(path.join(baseline,'src/context/cache/CachePlan.ts'),'utf8');
const sha=crypto.createHash('sha256').update(source).digest('hex');
if(sha!=='899b3f6973eb343d99647ce169a9076495fc935191def6f78ca14bf858b52dc7')throw new Error('Fixed CachePlan source required');
process.on('warning',warning=>{throw warning;});
const js=ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.ES2022,target:ts.ScriptTarget.ES2022}}).outputText;
const actual=await import('data:text/javascript;base64,'+Buffer.from(js).toString('base64'));
const tool=(name,description)=>({name,description,inputSchema:{type:'object'}});
const pair=[tool('\ud801','a'),tool('\ud800','z')];
const three=[...pair,tool('\ufffd','m')];
const cases=[
 ['opposed-description-pair',pair],
 ['reverse-opposed-description-pair',[...pair].reverse()],
 ['with-replacement-character',three],
 ['reverse-with-replacement-character',[...three].reverse()]
].map(([id,tools],index)=>{
 const input={provider:'fixture',model:'anthropic-fixture',systemPrompt:'system fixture',tools,messages:[{role:'user',content:[{type:'text',text:'first'}]}],enabled:true};
 const generation=90+index;
 return {id,input,generation,result:actual.buildCachePlan(input,generation)};
});
const oracle={baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,icu:process.versions.icu,typescript:ts.version,LANG:process.env.LANG,resolved:new Intl.Collator().resolvedOptions(),source:{path:'src/context/cache/CachePlan.ts',sha256:sha},cases};
fs.mkdirSync(out,{mode:0o700});
const bytes=JSON.stringify(oracle,null,2)+'\n';
fs.writeFileSync(path.join(out,'oracle.json'),bytes,{flag:'wx',mode:0o600});
console.log(JSON.stringify({cases:cases.length,sha256:crypto.createHash('sha256').update(bytes).digest('hex')}));
