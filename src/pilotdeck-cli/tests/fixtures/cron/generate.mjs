// Capture fixed-baseline CLI against a loopback fake, then normalize only UUID and cwd.
import http from 'node:http';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, writeFile, rm, realpath, rmdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const sha = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const source = `PilotDeck ${sha}, Node 22.22.0; mock TS matrix; uuid and cwd substituted`;
const cases = [
  {id:'list',args:['list','--history','--limit','12junk']},
  {id:'create',args:['create','--session','slack:abc','--message','hi','--once','later','--cron','ignored','--timezone','UTC']},
  {id:'delete',args:['delete','--task','actual','--stop-running']},
  {id:'stop',args:['stop','--run','run-1']},
  {id:'usage',args:['create']}, {id:'rpc-error',args:['list'],error:true},
  {id:'http-503',args:['create'],health:503}, {id:'hello-reject',args:['list'],rejectHello:true},
  {id:'no-token',args:['list'],noToken:true},
  {id:'create-explicit',args:['create','--session','abc','--message','hi','--once','now','--channel','custom','--project','/p']},
  {id:'create-plain',args:['create','--session','abc','--message','hi','--once','now']},
  {id:'list-invalid',args:['list','--limit','abc']},
  {id:'list-overflow',args:['list','--limit','9'.repeat(310)]},
  {id:'stop-empty',args:['stop','','--run','r']},
  {id:'create-empty',args:['create','--session','abc','--message','','--once','now']},
];
function normalize(raw) {
  const rows = raw.trimEnd().split('\n').map(line => JSON.parse(line));
  if (rows.length !== cases.length || rows.some((r, i) => r.id !== cases[i].id)) throw Error('wrong case set or order');
  const out = {};
  for (const row of rows) {
    if (!row.cwd.startsWith('/')) throw Error('missing absolute cwd');
    const wire = row.wire.map(text => {
      const frame = JSON.parse(text);
      if (Object.hasOwn(frame, 'id')) {
        if (typeof frame.id !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(frame.id)) throw Error('unexpected request id');
        frame.id = '<uuid>';
      }
      if (frame.params?.projectKey === row.cwd) frame.params.projectKey = '<cwd>';
      return frame;
    });
    out[row.id] = {stdout:row.stdout, stderr:row.stderr, native_exit:row.exit_before_cleanup,
      opened:row.opened, closed_before_cleanup:row.closed_before_cleanup, wire};
  }
  return `${JSON.stringify({source,cases:out}, null, 2)}\n`;
}
async function capture(project, node, directory) {
  project = await realpath(project); node = await realpath(node); directory = resolve(directory);
  if (execFileSync('git',['-C',project,'rev-parse','HEAD'],{encoding:'utf8'}).trim() !== sha) throw Error('TS checkout is not the pinned commit');
  if (execFileSync(node,['--version'],{encoding:'utf8'}).trim() !== 'v22.22.0') throw Error('Node v22.22.0 required');
  const { WebSocketServer } = await import(pathToFileURL(join(project,'node_modules/ws/wrapper.mjs')));
  await mkdir(directory); // exclusive; never overwrite prior evidence
  const root = await realpath(await mkdtemp(join(tmpdir(),'pdrs-cron-matrix-')));
  let active;
  const server=http.createServer((req,res)=>{ active.http.push({method:req.method,url:req.url}); res.writeHead(active.health ?? 200,{'content-length':'0'}); res.end(); });
  const wss=new WebSocketServer({server});
  wss.on('connection',ws=>{ const current=active; current.opened=true; ws.on('close',()=>{current.closed=true}); ws.on('message',raw=>{
    const frame=JSON.parse(String(raw)); current.wire.push(String(raw));
    if(frame.type==='hello') { if(current.rejectHello) ws.close(1008,'denied'); else ws.send(JSON.stringify({type:'hello_ok',protocolVersion:'1.2',serverVersion:'test',serverInfo:{mode:'remote',projectKey:'test',sessionCount:0,capabilities:[]}})); }
    if(frame.type==='request') ws.send(JSON.stringify(current.error ? {type:'response',id:frame.id,ok:false,error:{message:'fixture denied'}} : {type:'response',id:frame.id,ok:true,result:{ok:true}}));
  });});
  const records=[];
  const lease = '/tmp/pdrs-cron-18789.lock';
  const leaseDeadline = Date.now() + 60000;
  for (;;) {
    try { await mkdir(lease); break; }
    catch (e) {
      if (e.code !== 'EEXIST' || Date.now() >= leaseDeadline) throw e;
      await new Promise(ok => setTimeout(ok, 100));
    }
  }
  try {
    // Binding first fails closed if a user service or another test occupies the port.
    await new Promise((ok,fail)=>{server.once('error',fail);server.listen(18789,'127.0.0.1',ok)});
    for (const c of cases) {
      active={...c,http:[],wire:[],opened:false,closed:false};
      if(c.noToken) await rm(join(root,'server-token'),{force:true}); else await writeFile(join(root,'server-token'),'  test-token\n');
      const child=spawn(node,['--import',join(project,'node_modules/tsx/dist/loader.mjs'),join(project,'src/cli/pilotdeck.ts'),'cron',...c.args],{cwd:root,env:{HOME:root,PILOT_HOME:root,PATH:'/usr/bin:/bin',NODE_NO_WARNINGS:'1'}});
      let stdout='',stderr='',exit=null; child.stdout.on('data',b=>stdout+=b); child.stderr.on('data',b=>stderr+=b);
      child.on('exit',(code,signal)=>{exit={code,signal}}); const finished=new Promise(ok=>child.once('close',ok));
      await new Promise(ok=>setTimeout(ok,3000));
      records.push(JSON.stringify({id:c.id,cwd:root,http:active.http,wire:active.wire,opened:active.opened,closed_before_cleanup:active.closed,stdout,stderr,exit_before_cleanup:exit}));
      if(!exit) child.kill('SIGTERM'); await finished;
      for(const ws of wss.clients) ws.terminate();
    }
    const raw = `${records.join('\n')}\n`;
    await writeFile(join(directory,'raw.ndjson'),raw,{flag:'wx'});
    await writeFile(join(directory,'ts-oracle.json'),normalize(raw),{flag:'wx'});
  } finally {
    for(const ws of wss.clients) ws.terminate();
    wss.close(); server.close(); await rm(root,{recursive:true}); await rmdir(lease);
  }
}
const args=process.argv.slice(2);
if(args[0]==='--normalize' && args.length===3) {
  await writeFile(args[2],normalize(await readFile(args[1],'utf8')),{flag:'wx'});
} else if(args[0]==='--capture' && args.length===4) {
  await capture(args[1],args[2],args[3]);
} else {
  throw Error('usage: node generate.mjs --capture <pinned-ts-checkout> <node-22.22.0> <new-evidence-dir> | --normalize <raw.ndjson> <new-output.json>');
}
