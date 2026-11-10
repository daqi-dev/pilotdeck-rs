import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baselineArg,outArg]=process.argv.slice(2);
assert(baselineArg&&outArg,'usage: generate.mts BASELINE NEW_ORACLE_PATH');
const baseline=resolve(baselineArg),out=resolve(outArg);
assert.equal(process.versions.node,'22.22.0');assert.equal(process.versions.icu,'78.2');
const commit=assertBaseline(baseline);
const files=['src/always-on/runtime/discoveryPrompts.ts','src/always-on/runtime/discoveryPrompts.zh.ts','src/always-on/tool/AlwaysOnDiscoveryPlanTool.ts','src/always-on/tool/AlwaysOnReportTool.ts','src/always-on/tool/AlwaysOnWorkspaceTool.ts','src/always-on/tool/AlwaysOnChatHistoryTool.ts'];
const sourceSha256=await baselineSourceHashes(baseline,files);
const {buildDiscoveryPrompt}=await import(pathToFileURL(join(baseline,'src/always-on/runtime/discoveryPrompts.ts')).href);
const timestamps=[
 ['iso-ms3','2026-01-02T03:04:05.678Z'],
 ['iso-no-ms','2026-01-02T03:04:05Z'],
 ['iso-ms1','2026-01-02T03:04:05.6Z'],
 ['iso-ms2','2026-01-02T03:04:05.67Z'],
 ['iso-ms4','2026-01-02T03:04:05.6789Z'],
 ['non-digit-ms','2026-01-02T03:04:05.abcZ'],
 ['lowercase-z','2026-01-02T03:04:05.678z'],
 ['nonterminal-ms3','2026-01-02T03:04:05.678Z@@MODIFIED@@'],
];
const jsWhitespace='\t\v\f \u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff\r\n';
const prompts:Array<[string,string[]]>=[
 ['no-prompts',[]],
 ['empty-prompt',['']],
 ['whitespace-only',[jsWhitespace]],
 ['lf-flatten',[' \nfirst\nsecond\n ']],
 ['cr-preserved',[' \rleft\rright\r ']],
 ['ls-ps-preserved',[' \u2028left\u2028middle\u2029right\u2029 ']],
 ['mixed-lines',['\t first\nsecond\rthird\u2028fourth\u2029fifth \ufeff']],
 ['trim-all-js-whitespace',[jsWhitespace+'inside'+jsWhitespace]],
 ['literal-placeholders-lone-surrogates',['  \ud800@@HEAD@@\n@@DIGEST@@\udc00@@RUN@@  ','@@PLANS@@@@PROJECT@@@@CWD@@@@CREATED@@']],
 ['non-trim-codepoints',['\u180e\u200b kept \u200b\u180e']],
];
const cases:any[]=[];
for(const language of ['en-US','zh-CN'])for(const workspace of [false,true]){
 for(const [stampName,lastModified]of timestamps)for(const[promptName,userPrompts]of prompts){
  const input:any={
   language,projectRoot:'/fixture/@@CWD@@/项目😀',runId:'run@@CREATED@@\ud800',
   createdAt:'2026-01-02T03:04:05.678Z',chatDir:'/fixture/unused-chats',
   ...(workspace?{workspace:{cwd:' /workspace/@@PROJECT@@/\udc00 ',strategy:'git-worktree@@HEAD@@'}}:{}),
   chatDigest:{generatedAt:'2026-01-02T03:04:06.000Z',aliasMap:new Map(),sessions:[
    {sessionId:'native-one',alias:'alias@@PLANS@@\udc00',title:'  title@@DIGEST@@\ud800  ',lastModified,userPrompts},
    {sessionId:'native-two',alias:'second',title:'第二会话',lastModified:'2026-01-02T03:04:05.000Z',userPrompts:['  next\nline  ']},
   ]},
   existingPlans:[{id:'plan@@RUN@@',title:'plan@@CREATED@@\udc00',status:'scheduled@@STATUS@@',dedupeKey:'key@@TITLE@@\ud800'}],
  };
  cases.push({id:language+':'+workspace+':'+stampName+':'+promptName,input,expected:buildDiscoveryPrompt(input)});
 }
 for(const kind of ['digest-absent','digest-empty']){
  const input:any={language,projectRoot:'/empty/@@PROJECT@@',runId:'@@RUN@@',createdAt:'2026-01-02T03:04:05.678Z',chatDir:'/unused',
    ...(workspace?{workspace:{cwd:'/empty-workspace',strategy:'copy'}}:{}),
    ...(kind==='digest-empty'?{chatDigest:{generatedAt:'2026-01-02T03:04:05.000Z',sessions:[],aliasMap:new Map()}}:{}),
    existingPlans:[]};
  cases.push({id:language+':'+workspace+':'+kind,input,expected:buildDiscoveryPrompt(input)});
 }
}
const oracle={schema:1,sourceCommit:commit,node:process.versions.node,icu:process.versions.icu,sourceSha256,cases};
writeFileSync(out,JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
process.stdout.write(JSON.stringify({cases:cases.length,sha256:createHash('sha256').update(readFileSync(out)).digest('hex')})+'\n');
