import net from 'node:net';
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {createWriteStream} from 'node:fs';
import {createHash,randomBytes} from 'node:crypto';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {join,dirname,resolve} from 'node:path';
import {execFileSync} from 'node:child_process';
const here=dirname(fileURLToPath(import.meta.url));
if(process.argv.length!==5) throw Error('usage: node record.mjs BASELINE GATEWAY_BINARY NEW_OUTPUT');
const root=resolve(here,'../../../..');
const baseline=resolve(process.argv[2]),binary=resolve(process.argv[3]),out=resolve(process.argv[4]);
const candidateHead=execFileSync('git',['-C',root,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
execFileSync('git',['-C',root,'diff','--exit-code','--','src/pilotdeck-gateway/src','src/Cargo.toml','src/Cargo.lock']);
const sha=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
const binaryBefore=await sha(binary);
const baselineSourceBefore=await sha(join(baseline,'src/gateway/server/GatewayServer.ts'));
const baselineDistBefore=await sha(join(baseline,'dist/src/gateway/server/GatewayServer.js'));
await mkdir(dirname(out),{recursive:true});await mkdir(out,{recursive:false});
const runtime=join(out,'runtime');
const events=[],children=[],sockets=new Set(),servers=[],cleanupErrors=[];
const abort=new AbortController();
for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>abort.abort(Error('received '+signal)));
const fail=error=>abort.abort(error instanceof Error?error:Error(String(error)));
const rec=row=>events.push({...row,at:process.hrtime.bigint().toString()});
const deadline=(promise,label,ms=10000,ignoreAbort=false)=>new Promise((resolve,reject)=>{
 const finish=(fn,value)=>{clearTimeout(timer);abort.signal.removeEventListener('abort',onAbort);fn(value)};
 const onAbort=()=>finish(reject,abort.signal.reason);
 const timer=setTimeout(()=>finish(reject,Error(label+' timed out')),ms);
 promise.then(value=>finish(resolve,value),error=>finish(reject,error));
 if(!ignoreAbort){abort.signal.addEventListener('abort',onAbort,{once:true});if(abort.signal.aborted)onAbort()}
});
const listen=async server=>{server.listen(0,'127.0.0.1');await once(server,'listening');servers.push(server);return server.address().port};
const track=s=>{sockets.add(s);s.on('close',()=>sockets.delete(s));s.on('error',()=>{});return s};
const connect=async port=>{const s=track(net.connect(port,'127.0.0.1'));await deadline(once(s,'connect'),'connect');return s};
const frame=(body,op=1)=>{const p=Buffer.from(body);if(p.length>=126)throw Error('oversized probe payload');const mask=Buffer.from([11,22,33,44]);const m=Buffer.from(p);for(let i=0;i<m.length;i++)m[i]^=mask[i%4];return Buffer.concat([Buffer.from([128|op,128|p.length]),mask,m])};
const hello=frame(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'probe',token:'probe-token'}));
const initial=frame('INITIAL',9);
const send=(s,b)=>deadline(new Promise((resolve,reject)=>s.write(b,e=>e?reject(e):resolve())),'write');
function reader(socket) {
 let buf=Buffer.alloc(0),header=null;const frames=[],waiters=[];
 const notify=()=>{for(const w of [...waiters])if(w.predicate()){waiters.splice(waiters.indexOf(w),1);w.resolve()}};
 socket.on('data',b=>{try{buf=Buffer.concat([buf,b]);if(header===null){let p=buf.indexOf('\r\n\r\n');if(p<0)return;header=buf.subarray(0,p+4).toString();buf=buf.subarray(p+4);if(!header.startsWith('HTTP/1.1 101 '))throw Error(header)}
 while(buf.length>=2){let n=buf[1]&127,p=2;if(n===126){if(buf.length<4)break;n=buf.readUInt16BE(2);p=4}else if(n===127){if(buf.length<10)break;n=Number(buf.readBigUInt64BE(2));p=10}if(n>1024*1024)throw Error('oversized server frame');if(buf[1]&128)throw Error('masked server frame');if(buf.length<p+n)break;const payload=buf.subarray(p,p+n),op=buf[0]&15;let json=null;if(op===1)json=JSON.parse(payload);frames.push({op,payloadHex:payload.toString('hex'),json});buf=buf.subarray(p+n)}notify()}catch(error){fail(error)}});
 return {frames,wait:predicate=>predicate()?Promise.resolve():deadline(new Promise(resolve=>waiters.push({predicate,resolve})),'server frame'),get header(){return header}};
}
async function child(name,argv,env){const log=createWriteStream(join(out,name+'.log'),{flags:'wx'});const p=spawn(argv[0],argv.slice(1),{cwd:join(runtime,'project'),env,detached:true,stdio:['ignore','pipe','pipe']});p.stdout.pipe(log);p.stderr.pipe(log);children.push({name,p,log,argv});await once(p,'spawn');return p}
async function ready(port,p){for(let i=0;i<150;i++){if(abort.signal.aborted)throw abort.signal.reason;if(p.exitCode!==null)throw Error('early exit '+p.exitCode);try{const r=await fetch('http://127.0.0.1:'+port+'/health',{signal:AbortSignal.timeout(200),headers:{Connection:'close'}});if(r.ok)return}catch{}await new Promise(r=>setTimeout(r,50))}throw Error('health timeout')}
const env=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('PILOTDECK_')&&!k.startsWith('GP_')&&!['PILOT_HOME','DATABASE_PATH','NODE_OPTIONS','WORKSPACES_ROOT'].includes(k)));
let status='FAIL',error=null,results=[];
try{
 await mkdir(runtime,{recursive:false});for(const d of ['home','project','tmp','workspaces','xdg'])await mkdir(join(runtime,d));
 await writeFile(join(runtime,'home','pilotdeck.yaml'),`schemaVersion: 1
agent:
  model: parity/deterministic
  maxContextTokens: 65536
  maxOutputTokens: 8192
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:9/v1
      apiKey: offline-probe-only
      models:
        deterministic:
          capabilities:
            supportsToolUse: true
            maxContextTokens: 65536
            maxOutputTokens: 8192
telemetry:
  enabled: false
cron:
  enabled: false
webui:
  runtime:
    host: 127.0.0.1
    databasePath: ${JSON.stringify(join(runtime,'home/auth.db'))}
    workspacesRoot: ${JSON.stringify(join(runtime,'workspaces'))}
`);
 Object.assign(env,{HOME:join(runtime,'home'),PILOT_HOME:join(runtime,'home'),TMPDIR:join(runtime,'tmp'),XDG_CONFIG_HOME:join(runtime,'xdg'),XDG_CACHE_HOME:join(runtime,'xdg'),XDG_DATA_HOME:join(runtime,'xdg'),DATABASE_PATH:join(runtime,'home','auth.db'),WORKSPACES_ROOT:join(runtime,'workspaces'),PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_TOKEN:'probe-token',PILOTDECK_SKIP_BROWSER_OPEN:'1',GP_POLL_WATCH:'1',PDRS_UPGRADE_OBSERVATION:join(out,'ts-observed.jsonl')});
 // Bind a private ephemeral port before launching, then release this owned reservation.
 const reserve=net.createServer();const tsPort=await listen(reserve);await new Promise(r=>reserve.close(r));servers.splice(servers.indexOf(reserve),1);
 env.PILOTDECK_GATEWAY_PORT=String(tsPort);
 const ts=await child('baseline',['node','--import',join(root,'scripts/gateway-parity/watcher.mjs'),'--import',join(here,'observe.mjs'),join(baseline,'dist/src/cli/pilotdeck.js'),'server'],env);await ready(tsPort,ts);
 for(const side of ['ts','rust'])for(const head of ['empty','full','partial'])for(const split of [false,true]){
  const id=side+'-'+head+'-'+(split?'split':'whole');let held=[],release=null,arrived;
  const barrier=new Promise(r=>arrived=r);let relayClients=0;
  const relay=net.createServer(c=>{relayClients++;track(c);const u=track(net.connect(tsPort,'127.0.0.1'));c.on('data',b=>u.write(b));c.on('close',()=>u.destroy());u.on('close',()=>c.destroy());let headers=Buffer.alloc(0),captured=false,open=false;
   release=()=>{if(open)throw Error('double release');open=true;rec({id,event:'release-101'});for(const b of held)c.write(b);held=[]};
   u.on('data',b=>{try{if(open){c.write(b);return}held.push(Buffer.from(b));if(!captured){headers=Buffer.concat([headers,b]);const p=headers.indexOf('\r\n\r\n');if(p>=0){if(!headers.toString().startsWith('HTTP/1.1 101 ')){open=true;for(const h of held)c.write(h);held=[];return}captured=true;rec({id,event:'real-ts-101-held'});arrived()}}}catch(error){fail(error)}});
  });const relayPort=await listen(relay);
  let port=relayPort,rs=null;
  if(side==='rust'){const reservation=net.createServer();port=await listen(reservation);await new Promise(r=>reservation.close(r));servers.splice(servers.indexOf(reservation),1);rs=await child(id,[binary],{...env,PILOTDECK_PROXY_LISTEN:'127.0.0.1:'+port,PILOTDECK_PROXY_UPSTREAM:'http://127.0.0.1:'+relayPort});await ready(port,rs)}
  const c=await connect(port),r=reader(c);
  const tail=head==='empty'?Buffer.alloc(0):head==='full'?initial:initial.subarray(0,3);
  const nonce=randomBytes(16).toString('base64');
  const request=Buffer.from('GET /ws?probe='+id+' HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+nonce+'\r\nSec-WebSocket-Version: 13\r\n\r\n');
  await send(c,Buffer.concat([request,tail]));await deadline(barrier,'held real 101');
  if(split){await send(c,hello.subarray(0,3));await send(c,hello.subarray(3))}else await send(c,hello);
  rec({id,event:'later-client-write-complete-before-release',bytes:hello.length});release();
  await r.wait(()=>r.frames.some(f=>f.json?.type==='hello_ok'));
  await send(c,frame(JSON.stringify({type:'request',id:'post',method:'describe_server'})));
  await r.wait(()=>r.frames.some(f=>f.json?.id==='post'));
  await send(c,frame('FENCE',9));await r.wait(()=>r.frames.some(f=>f.op===10&&Buffer.from(f.payloadHex,'hex').toString()==='FENCE'));
  if(r.frames.filter(f=>f.json?.type==='hello_ok').length!==1||r.frames.filter(f=>f.json?.id==='post').length!==1||r.frames.filter(f=>f.op===10&&Buffer.from(f.payloadHex,'hex').toString()==='FENCE').length!==1||r.frames.some(f=>f.op===10&&f.payloadHex===Buffer.from('INITIAL').toString('hex')))throw Error('duplicate/lost or initial head processed '+id);
  rec({id,event:'validated'});results.push({id,side,initialTailHex:tail.toString('hex'),laterSplit:split,frames:r.frames,relayConnections:relayClients});c.destroy();await once(c,'close');await new Promise(r=>relay.close(r));servers.splice(servers.indexOf(relay),1);
  if(rs){process.kill(-rs.pid,'SIGTERM');await deadline(once(rs,'exit'),'Rust stop')}
 }
 const observations=(await readFile(join(out,'ts-observed.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
 for(const result of results){const o=observations.filter(o=>o.event==='upgrade'&&o.url==='/ws?probe='+result.id);if(o.length!==1)throw Error('upgrade observation missing '+result.id);result.actualTsHeadHex=o[0].headHex;if(result.side==='ts'&&o[0].headHex!==result.initialTailHex)throw Error('TS read boundary did not match '+result.id);if(result.side==='rust'&&o[0].headBytes!==0)throw Error('Rust forwarded initial head '+result.id)}
 if(results.length!==12)throw Error('incomplete matrix');status='PASS';
}catch(e){error=e.stack??String(e)}finally{
 for(const s of sockets)s.destroy();for(const s of servers)try{await deadline(new Promise(r=>s.close(()=>r())),'listener cleanup',3000,true)}catch(error){cleanupErrors.push(String(error))};
 for(const x of [...children].reverse()){if(x.p.exitCode===null&&x.p.signalCode===null){try{process.kill(-x.p.pid,'SIGTERM')}catch{}try{await deadline(once(x.p,'exit'),'child stop',3000,true)}catch{try{process.kill(-x.p.pid,'SIGKILL')}catch{}try{await deadline(once(x.p,'exit'),'child kill',3000,true)}catch(error){cleanupErrors.push(String(error))}}}x.log.end()}
 const binaryAfter=await sha(binary),baselineSourceAfter=await sha(join(baseline,'src/gateway/server/GatewayServer.ts')),baselineDistAfter=await sha(join(baseline,'dist/src/gateway/server/GatewayServer.js'));
 if(binaryBefore!==binaryAfter||baselineSourceBefore!==baselineSourceAfter||baselineDistBefore!==baselineDistAfter){status='FAIL';error='binary or baseline changed during capture'}
 if(cleanupErrors.length||children.some(x=>x.p.exitCode===null&&x.p.signalCode===null)){status='FAIL';error??='cleanup incomplete'}
 const report={status,error,cleanupErrors,scope:'actual default baseline CLI/GatewayServer HTTP entry and exact candidate binary; initial Rust read ownership proved separately by scripted production-path unit tests',results,events,children:children.map(({name,p,argv})=>({name,pid:p.pid,argv,exitCode:p.exitCode,signalCode:p.signalCode,stopped:p.exitCode!==null||p.signalCode!==null})),candidateRoot:root,candidateHead,candidateBinary:binary,binarySha256Before:binaryBefore,binarySha256After:binaryAfter,baselineSourceSha256Before:baselineSourceBefore,baselineSourceSha256After:baselineSourceAfter,baselineDistSha256Before:baselineDistBefore,baselineDistSha256After:baselineDistAfter};
 await writeFile(join(out,'report.json'),JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({status,cases:results.length,error}));if(status!=='PASS')process.exitCode=1;
}
