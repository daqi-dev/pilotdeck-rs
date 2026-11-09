import http from 'node:http';
import {pathToFileURL} from 'node:url';
import {spawn,execFileSync} from 'node:child_process';
import {mkdtemp,mkdir,writeFile,readFile,rm,realpath,rmdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
const [mode, projectArg, nodeArg, outputArg] = process.argv.slice(2);
if (!['--capture','--boundary'].includes(mode) || !projectArg || !nodeArg || !outputArg) throw Error('usage: node generate.mjs --capture <pinned-ts-checkout> <node-22.22.0> <new-evidence-dir>');
const project = await realpath(projectArg), node = await realpath(nodeArg);
if(execFileSync(node,['--version'],{encoding:'utf8'}).trim()!=='v22.22.0') throw Error('Node v22.22.0 required');
const {WebSocketServer}=await import(pathToFileURL(join(project,'node_modules/ws/wrapper.mjs')));
await mkdir(outputArg);
const raw=[];
if(execFileSync('git',['-C',project,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85') throw Error('wrong TS baseline');
const root=await realpath(await mkdtemp(join(tmpdir(),'pdrs-cli-channel-'))), home=join(root,'home'), cwd=join(root,'project');
await mkdir(home); await mkdir(cwd); await writeFile(join(home,'server-token'),'channel-fake-token\n');
await writeFile(join(home,'pilotdeck.yaml'),'schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: fake-only\n      models:\n        test: {}\n');
let current;
const server=http.createServer((req,res)=>{current.http.push({method:req.method,url:req.url});res.writeHead(200,{'content-length':'0'});res.end()});
const wss=new WebSocketServer({server});
wss.on('connection',ws=>{let c=current;c.opened=true;ws.on('close',()=>c.closed=true);ws.on('message',raw=>{const f=JSON.parse(String(raw));c.wire.push(f);if(f.type==='hello')ws.send(JSON.stringify({type:'hello_ok',protocolVersion:'1.2',serverVersion:'test',serverInfo:{mode:'remote',projectKey:cwd,sessionCount:0,capabilities:[]}}));
if(f.type==='request'){
if(c.drop){ws.close(1011,'fixture');return;}
const events=mode==='--boundary' ? (c.finalOnly?[]:[{type:'assistant_text_delta',text:'A'}]) : [{type:'assistant_text_delta',text:'A'}, {type:'assistant_thinking_delta',text:'h'}, {type:'tool_call_started',name:'read',argsPreview:'p'}, {type:'tool_call_finished',toolCallId:'t',ok:true,resultPreview:'yes'}, {type:'permission_request',toolName:'read',requestId:'r'}, {type:'structured_output',payload:{x:1}}, {type:'plan_mode_changed',mode:'plan'}, {type:'error',code:'E',message:'bad'}, {type:'unknown',text:'ignored'}];
for(let i=0;i<events.length;i++)ws.send(JSON.stringify({type:'event',id:f.id,seq:i,final:false,event:events[i]}));
ws.send(JSON.stringify({type:'event',id:f.id,seq:events.length,final:true,event:{type:'assistant_text_delta',text:'FINAL-OMIT'}}));
}})});
const lease='/tmp/pdrs-cron-18789.lock'; const end=Date.now()+60000;
for(;;){try{await mkdir(lease);break;}catch(error){if(error.code!=='EEXIST'||Date.now()>=end)throw error;await new Promise(ok=>setTimeout(ok,100));}}
try {
await new Promise((ok,fail)=>{server.once('error',fail);server.listen(18789,'127.0.0.1',ok)});
const cases=mode==='--boundary' ? [{id:'bare-cr-eof',args:[],stdin:'one\rtwo\r\nthree'},{id:'final-only',args:['go'],stdin:'',finalOnly:true},{id:'disconnect',args:['go'],stdin:'',drop:true},{id:'broken-stdout',args:['go'],stdin:'',broken:true}] : [{id:'argv',args:['hello','--strange',' world '],stdin:''},{id:'lines',args:[],stdin:'  first  \r\n\nsecond\n'},{id:'whitespace',args:['  '],stdin:'   \n'}];
for(const c of cases){current={...c,http:[],wire:[],opened:false,closed:false};let out='',err='',exit=null;let child=spawn(node,['--import',join(project,'node_modules/tsx/dist/loader.mjs'),join(project,'src/cli/pilotdeck.ts'),...c.args],{cwd,env:{HOME:home,PILOT_HOME:home,PATH:'/usr/bin:/bin',NODE_NO_WARNINGS:'1'},stdio:['pipe','pipe','pipe']});child.stdin.end(c.stdin);if(c.broken)child.stdout.destroy();else child.stdout.on('data',x=>out+=x);child.stderr.on('data',x=>err+=x);child.on('exit',(code,signal)=>exit={code,signal});let done=new Promise(ok=>child.once('close',ok));await new Promise(ok=>setTimeout(ok,4500));raw.push({id:c.id,cwd,argv:c.args,stdin:c.stdin,http:current.http,wire:current.wire,opened:current.opened,closed_before_cleanup:current.closed,stdout:out,stderr:err,exit_before_cleanup:exit});if(!exit)child.kill('SIGTERM');await done;for(const ws of wss.clients)ws.terminate();}
const source='PilotDeck cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85, Node 22.22.0; fake remote gateway; request UUID and cwd substituted';
const casesOut={}; for(const row of raw){const wire=row.wire.map(frame=>{const f=structuredClone(frame);if(f.type==='request'){
if(!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(f.id))throw Error('invalid UUID');f.id='<uuid>';
const expected=`cli:project=${row.cwd}:default`;if(f.params.sessionKey!==expected)throw Error('unexpected session');f.params.sessionKey='cli:project=<cwd>:default';}return f;});
if(mode==='--boundary') { const marker=row.id==='broken-stdout'?'Error: write EPIPE':row.id==='disconnect'?'Gateway WebSocket closed.':null; if(marker&&!row.stderr.includes(marker)) throw Error('expected stderr evidence missing'); casesOut[row.id]={argv:row.argv,stdin:row.stdin,wire,stdout:row.stdout,native_exit:row.exit_before_cleanup,closed_before_cleanup:row.closed_before_cleanup,stderr_contains:marker}; }
else casesOut[row.id]={argv:row.argv,stdin:row.stdin,wire,stdout:row.stdout,stderr:row.stderr,native_exit:row.exit_before_cleanup,closed_before_cleanup:row.closed_before_cleanup};}
await writeFile(join(outputArg,'raw.ndjson'),raw.map(row=>JSON.stringify(row)).join('\n')+'\n',{flag:'wx'});
await writeFile(join(outputArg,mode==='--boundary'?'boundary-oracle.json':'ts-oracle.json'),JSON.stringify({source,cases:casesOut},null,2)+'\n',{flag:'wx'});
} finally {wss.close();server.close();await rm(root,{recursive:true});await rmdir(lease);}

