import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {createRequire} from 'node:module';
const [baseline,out,seed]=process.argv.slice(2);
if(!baseline||!out||!seed)throw new Error('Usage: capture.mjs BASELINE OUT SEED');
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw new Error('Fixed Node/ICU required');
const require=createRequire(path.join(baseline,'package.json'));
const ts=require('typescript');if(ts.version!=='5.9.3')throw new Error('Fixed TypeScript required');
const sourcePath=path.join(baseline,'src/context/cache/CachePlan.ts');
const source=fs.readFileSync(sourcePath,'utf8');
const sourceSha=crypto.createHash('sha256').update(source).digest('hex');
const expected=JSON.parse(fs.readFileSync(seed,'utf8'));
if(sourceSha!==expected.cachePlanSha)throw new Error('Source hash differs');
process.on('warning',warning=>{throw warning;});
const js=ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.ES2022,target:ts.ScriptTarget.ES2022}}).outputText;
const actual=await import('data:text/javascript;base64,'+Buffer.from(js).toString('base64'));
const schema={type:'object',properties:{value:{type:'string'}}};
const names=['z','Z','alpha','Alpha','a_2','a-2','a2','a10','é','e\u0301','Å','ä','Ö','ø','İ','I','ı','😀','𝒜','\ud800','\ud801'];
const tools=names.map((name,index)=>({name,description:'fixture '+index,inputSchema:schema}));
const duplicate=['z','a','é','e\u0301','A','a','😀','\ud800'].map((description,index)=>({name:'same',description,inputSchema:{type:'object',properties:{['p'+index]:{type:'string'}},required:[]}}));
const messages=[{role:'user',content:[{type:'text',text:'first'}]},{role:'system',content:[{type:'text',text:'excluded'}]},{role:'assistant',content:[{type:'text',text:'second'}]},{role:'user',content:[{type:'text',text:'third'}]},{role:'assistant',content:[{type:'text',text:'fourth'}]}];
const cases=[
{id:'unicode-names',tools},{id:'reverse-unicode-names',tools:[...tools].reverse()},
{id:'duplicate-names',tools:duplicate},{id:'reverse-duplicate-names',tools:[...duplicate].reverse()},
{id:'actual-native-catalog',tools:expected.tools},{id:'empty',tools:[]},{id:'single',tools:[tools[0]]}
].map(({id,tools},index)=>{const input={provider:'fixture',model:'anthropic-fixture',systemPrompt:'system fixture',tools,messages,enabled:true};return{id,input,generation:9+index,result:actual.buildCachePlan(input,9+index)};});
const strings=[...new Set(cases.flatMap(row=>row.input.tools.flatMap(tool=>[tool.name,actual.stableSerialize(tool)])))];
const signs=strings.map(left=>strings.map(right=>Math.sign(left.localeCompare(right))));
fs.mkdirSync(out,{mode:0o700});
const result={baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,icu:process.versions.icu,typescript:ts.version,LANG:process.env.LANG??null,resolved:new Intl.Collator().resolvedOptions(),source:{path:'src/context/cache/CachePlan.ts',sha256:sourceSha},cases,strings,signs};
fs.writeFileSync(path.join(out,'oracle.json'),JSON.stringify(result,null,2)+'\n',{flag:'wx',mode:0o600});
console.log(JSON.stringify({cases:cases.length,strings:strings.length,pairs:strings.length**2,locale:result.resolved.locale,sha256:crypto.createHash('sha256').update(fs.readFileSync(path.join(out,'oracle.json'))).digest('hex')}));
