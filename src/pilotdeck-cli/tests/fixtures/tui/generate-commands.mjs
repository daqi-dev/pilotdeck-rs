// Capture the actual fixed TS handleCommand body with controlled callbacks.
import { readFile, mkdir, writeFile, realpath } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import vm from 'node:vm';
const [checkoutArg, outArg] = process.argv.slice(2);
if (!checkoutArg || !outArg || process.argv.length !== 4) throw Error('usage: node capture.mjs <baseline> <new-output-dir>');
if (process.version !== 'v22.22.0' || process.versions.icu !== '78.2') throw Error('Node 22.22.0 / ICU 78.2 required');
const checkout=await realpath(checkoutArg);
const expectedCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const commit=execFileSync('git',['-C',checkout,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
if(commit!==expectedCommit)throw Error('baseline HEAD mismatch');
const path=join(checkout,'src/adapters/channel/tui/app/TuiApp.tsx');
const source=await readFile(path,'utf8');
const sha=x=>createHash('sha256').update(x).digest('hex');
const sourceSha256=sha(source);
if(sourceSha256!=='0134f0d8865eac294864ec41fd229fa3cf0d8032125a90437cc2c8ada556470b')throw Error('source hash mismatch');
const start='async function handleCommand('; const end='\nfunction buildPermissionEntry(';
if(source.split(start).length!==2 || source.split(end).length!==2)throw Error('non-unique extraction anchors');
const a=source.indexOf(start), b=source.indexOf(end,a);
if(b<=a)throw Error('bad extraction bounds');
const extracted=source.slice(a,b);
const require=createRequire(join(checkout,'package.json')); const ts=require('typescript');
const built=ts.transpileModule(`${extracted}\nglobalThis.__handleCommand = handleCommand;`, {compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.None},reportDiagnostics:true});
if((built.diagnostics??[]).some(x=>x.category===ts.DiagnosticCategory.Error))throw Error('TypeScript transpile failed');
const out=resolve(outArg);await mkdir(out);
const cases=[
 ['ordinary','hello'],['trim','  /help  '],['uppercase','/HELP'],['plain-slash','/'],
 ['new','/new'],['sessions','/sessions'],['dashboard','/dashboard'],['switch-missing','/switch'],
 ['switch-prefix','/switch 2xyz'],['switch-plus','/switch +1'],['switch-negative','/switch -2'],
 ['switch-huge','/switch 9999999999999999999999999999999999'],['switch-space','/switch\u00a0\t2'],
 ['permission-show','/permissions'],['permission-allow','/permissions allow  bash:git\t status'],
 ['permission-deny','/permissions deny read_file'],['permission-clear','/permissions clear bash:git'],
 ['permission-bypass','/permissions bypass ignored'],['permission-usage','/permissions allow'],
 ['permission-unknown-usage','/permissions surprise'],['permission-unknown','/permissions surprise xyz'],
 ['permission-case','/permissions Allow foo'],['mode-plan','/mode plan'],['mode-bypass','/mode bypassPermissions'],
 ['mode-unknown','/mode PLAN'],['mode-empty','/mode'],['view-first','/view 1'],['view-last','/view 900'],
 ['view-invalid','/view 1.5'],['view-none','/view 2','no-tools'],['clear','/clear'],['help','/help'],['exit','/exit'],
 ['unknown','/newer x'],['tabs','  /permissions\u2003allow\u00a0hello\tworld  '],
];
const rows=[];
for(const [id,raw,variant] of cases){
 let state={messages:[{role:'user',text:'question'}],mode:'default',activeSessionKey:'old',sessions:[],focusedIndex:3,helpOpen:false};
 let settings={skipPermissions:true,allowedTools:['bash:git'],disallowedTools:['read_file']}; const calls=[];
 const messages=variant==='no-tools'?[]:[{role:'tool',text:'short',lineCount:1},{role:'tool',text:'first',lineCount:8},{role:'tool',text:'last',resultPath:'/fixture'}];
 const context=vm.createContext({readPermissionSettings:()=>{calls.push({op:'readPermissions'});return structuredClone(settings)},writePermissionSettings:x=>{calls.push({op:'writePermissions',value:x});settings={...settings,...x}}});
 vm.runInContext(built.outputText,context,{timeout:1000,filename:'fixed-TuiApp-handleCommand.js'});
 const gateway={newSession:async x=>{calls.push({op:'newSession',value:x});return {sessionKey:'new-session'}},listSessions:async x=>{calls.push({op:'listSessions',value:x});return {sessions:[{sessionKey:'s1',summary:'first'},{sessionId:'s2',summary:'second'}]}}};
 const setState=fn=>{state=fn(state);calls.push({op:'setState'})};
 const exit=()=>calls.push({op:'exit'}), viewer=(content,title)=>calls.push({op:'viewer',content,title});
 const openToolOutput=async m=>calls.push({op:'openToolOutput',value:m});
 const sidebar=async()=>calls.push({op:'openSidebar'}), switchSession=async(key,summary)=>calls.push({op:'switchToSession',key,summary});
 const trimmed=raw.trim();
 const handled=trimmed ? await context.__handleCommand(trimmed,gateway,'/fixture-project',setState,exit,viewer,openToolOutput,messages,sidebar,switchSession) : null;
 rows.push({id,raw,trimmed,handled,calls,state,settings});
}
const output={sourceCommit:commit,sourceSha256,extraction:{start,end,startLine:source.slice(0,a).split('\n').length,endLine:source.slice(0,b).split('\n').length,extractedSha256:sha(extracted),transpiledSha256:sha(built.outputText)},runtime:{node:process.version,icu:process.versions.icu,typescript:ts.version},assumptions:{projectKey:'/fixture-project',sessions:['s1','s2'],permissionState:'controlled in-memory',submitTrim:'caller raw.trim() before handleCommand',effects:'injected fakes; no network or filesystem writes'},cases:rows};
await writeFile(join(out,'commands.json'),JSON.stringify(output,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:rows.length,sourceSha256,extractedSha256:output.extraction.extractedSha256}));
