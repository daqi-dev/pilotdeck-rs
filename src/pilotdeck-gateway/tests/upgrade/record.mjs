import net from 'node:net';
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {createWriteStream} from 'node:fs';
import {createHash,randomBytes} from 'node:crypto';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {join,dirname,resolve} from 'node:path';
import {execFileSync} from 'node:child_process';
import {checkSuffix} from './check_suffix.mjs';
import {captureSourceIdentity,verifySourceIdentity} from './source_identity.mjs';
const here=dirname(fileURLToPath(import.meta.url));
if(process.argv.length!==5) throw Error('usage: node record.mjs BASELINE GATEWAY_BINARY NEW_OUTPUT');
const root=resolve(here,'../../../..');
const baseline=resolve(process.argv[2]),binary=resolve(process.argv[3]),out=resolve(process.argv[4]);
const sourceBefore=captureSourceIdentity(root);
const candidateHead=sourceBefore.head;
const sourceIdentity={version:1,before:sourceBefore,after:null,verified:false,artifactOrigin:'CALLER_CARGO_ARTIFACT_EVIDENCE_REQUIRED'};
const sha=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
const binaryBefore=await sha(binary);
const baselineSourceBefore=await sha(join(baseline,'src/gateway/server/GatewayServer.ts'));
const baselineDistBefore=await sha(join(baseline,'dist/src/gateway/server/GatewayServer.js'));
await mkdir(dirname(out),{recursive:true});await mkdir(out,{recursive:false});
const runtime=join(out,'runtime');
await writeFile(join(out, 'ts-observed.jsonl'), '', {flag: 'wx'});
const events=[],children=[],sockets=new Set(),servers=[],cleanupErrors=[];
const configurationBindings=[],suffixResults=[],routingResults=[];
const abort=new AbortController();
for(const signal of ['SIGINT','SIGTERM'])process.once(signal,()=>abort.abort(Error('received '+signal)));
const fail=error=>abort.abort(error instanceof Error?error:Error(String(error)));
const rec=row=>events.push({...row,at:process.hrtime.bigint().toString()});
function deadline(promise, label, ms = 10000, ignoreAbort = false) {
 return new Promise((resolve, reject) => {
  const finish = (complete, value) => {
   clearTimeout(timer);
   abort.signal.removeEventListener('abort', onAbort);
   complete(value);
  };
  const onAbort = () => finish(reject, abort.signal.reason);
  const timer = setTimeout(() => finish(reject, Error(label + ' timed out')), ms);
  promise.then(value => finish(resolve, value), error => finish(reject, error));
  if (!ignoreAbort) {
   abort.signal.addEventListener('abort', onAbort, {once: true});
   if (abort.signal.aborted) onAbort();
  }
 });
}
async function listen(server) {
 server.listen(0, '127.0.0.1');
 await once(server, 'listening');
 servers.push(server);
 return server.address().port;
}
function track(socket) {
 sockets.add(socket);
 socket.on('close', () => sockets.delete(socket));
 socket.on('error', () => {});
 return socket;
}
async function connect(port) {
 const socket = track(net.connect(port, '127.0.0.1'));
 await deadline(once(socket, 'connect'), 'connect');
 return socket;
}
function frame(body, opcode = 1) {
 const payload = Buffer.from(body);
 if (payload.length >= 126) throw Error('oversized probe payload');
 const mask = Buffer.from([11, 22, 33, 44]), masked = Buffer.from(payload);
 for (let index = 0; index < masked.length; index++) masked[index] ^= mask[index % 4];
 return Buffer.concat([Buffer.from([128 | opcode, 128 | payload.length]), mask, masked]);
}
const hello = frame(JSON.stringify({type: 'hello', protocolVersion: '1.2', clientName: 'web',
 clientVersion: 'probe', token: 'probe-token'}));
const initial = frame('INITIAL', 9);
function send(socket, bytes) {
 return deadline(new Promise((resolve, reject) => {
  socket.write(bytes, error => error ? reject(error) : resolve());
 }), 'write');
}
function reader(socket) {
 let buffer = Buffer.alloc(0), header = null;
 const frames = [], waiters = [], raw = [];
 function notify() {
  for (const waiter of [...waiters]) {
   if (waiter.predicate()) {
    waiters.splice(waiters.indexOf(waiter), 1);
    waiter.resolve();
   }
  }
 }
 socket.on('data', bytes => {
  try {
   raw.push(Buffer.from(bytes));
   buffer = Buffer.concat([buffer, bytes]);
   if (header === null) {
    const end = buffer.indexOf('\r\n\r\n');
    if (end < 0) return;
    header = buffer.subarray(0, end + 4).toString();
    buffer = buffer.subarray(end + 4);
    if (!header.startsWith('HTTP/1.1 101 ')) throw Error(header);
   }
   while (buffer.length >= 2) {
    let size = buffer[1] & 127, offset = 2;
    if (size === 126) {
     if (buffer.length < 4) break;
     size = buffer.readUInt16BE(2); offset = 4;
    } else if (size === 127) {
     if (buffer.length < 10) break;
     size = Number(buffer.readBigUInt64BE(2)); offset = 10;
    }
    if (size > 1024 * 1024) throw Error('oversized server frame');
    if (buffer[1] & 128) throw Error('masked server frame');
    if (buffer.length < offset + size) break;
    const payload = buffer.subarray(offset, offset + size), opcode = buffer[0] & 15;
    frames.push({op: opcode, payloadHex: payload.toString('hex'),
     json: opcode === 1 ? JSON.parse(payload) : null});
    buffer = buffer.subarray(offset + size);
   }
   notify();
  } catch (error) { fail(error); }
 });
 return {
  frames,
  get rawHex() { return Buffer.concat(raw).toString('hex'); },
  get header() { return header; },
  wait: predicate => predicate() ? Promise.resolve() : deadline(new Promise(resolve => {
   waiters.push({predicate, resolve});
  }), 'server frame'),
 };
}
async function child(name, argv, env, cwd = join(runtime, 'project')) {
 const log = createWriteStream(join(out, name + '.log'), {flags: 'wx'});
 log.on('error', fail);
 const processHandle = spawn(argv[0], argv.slice(1), {
  cwd, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
 });
 // Final cleanup owns the log stream. Either stdio pipe may end first.
 processHandle.stdout.pipe(log, {end: false});
 processHandle.stderr.pipe(log, {end: false});
 const closed = new Promise(resolve => processHandle.once('close', resolve));
 children.push({name, p: processHandle, log, argv, closed});
 await once(processHandle, 'spawn');
 return processHandle;
}
async function ready(port,p){for(let i=0;i<150;i++){if(abort.signal.aborted)throw abort.signal.reason;if(p.exitCode!==null)throw Error('early exit '+p.exitCode);try{const r=await fetch('http://127.0.0.1:'+port+'/health',{signal:AbortSignal.timeout(200),headers:{Connection:'close'}});if(r.ok)return}catch{}await new Promise(r=>setTimeout(r,50))}throw Error('health timeout')}
const env=Object.fromEntries(Object.entries(process.env).filter(([k])=>!k.startsWith('PILOTDECK_')&&!k.startsWith('GP_')&&!['PILOT_HOME','DATABASE_PATH','NODE_OPTIONS','WORKSPACES_ROOT'].includes(k)));


function baselineArgv() {
 return [process.execPath, '--import', join(root, 'scripts/gateway-parity/watcher.mjs'),
  '--import', join(here, 'observe.mjs'), join(baseline, 'dist/src/cli/pilotdeck.js'), 'server'];
}
async function availablePort() {
 const reservation = net.createServer();
 const port = await listen(reservation);
 await new Promise(resolve => reservation.close(resolve));
 servers.splice(servers.indexOf(reservation), 1);
 return port;
}
async function stopChild(processHandle) {
 if (processHandle.exitCode !== null || processHandle.signalCode !== null) return;
 const stopped = once(processHandle, 'exit');
 try { process.kill(-processHandle.pid, 'SIGTERM'); } catch (error) {
  if (error.code !== 'ESRCH') throw error;
 }
 await deadline(stopped, 'child stop');
}
async function readObservations() {
 return (await readFile(join(out, 'ts-observed.jsonl'), 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);
}
async function observation(predicate) {
 const stopAt = Date.now() + 10000;
 while (Date.now() < stopAt) {
  const found = (await readObservations()).find(predicate);
  if (found) return found;
  if (abort.signal.aborted) throw abort.signal.reason;
  // Poll for an actual event, never infer protocol behavior from elapsed time.
  await new Promise(resolve => setTimeout(resolve, 20));
 }
 throw Error('baseline exception observation timed out');
}

async function provision(runtime) {
 await mkdir(runtime,{recursive:true});for(const d of ['home','project','tmp','workspaces','xdg'])await mkdir(join(runtime,d));
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
 const isolatedEnv={...env,HOME:join(runtime,'home'),PILOT_HOME:join(runtime,'home'),TMPDIR:join(runtime,'tmp'),XDG_CONFIG_HOME:join(runtime,'xdg'),XDG_CACHE_HOME:join(runtime,'xdg'),XDG_DATA_HOME:join(runtime,'xdg'),DATABASE_PATH:join(runtime,'home','auth.db'),WORKSPACES_ROOT:join(runtime,'workspaces'),PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_TOKEN:'probe-token',PILOTDECK_SKIP_BROWSER_OPEN:'1',GP_POLL_WATCH:'1',PDRS_UPGRADE_OBSERVATION:join(out,'ts-observed.jsonl')};
 const configPath=join(runtime,'home','pilotdeck.yaml');
 configurationBindings.push({path:configPath,sha256Before:await sha(configPath)});
 return isolatedEnv;
}
const inputPaths = {
 recorder: fileURLToPath(import.meta.url), observer: join(here, 'observe.mjs'),
 suffixChecker: join(here, 'check_suffix.mjs'),
 sourceIdentityHelper: join(here,'source_identity.mjs'),
 watcher: join(root, 'scripts/gateway-parity/watcher.mjs'), nodeExecutable: process.execPath,
 baselineCLI: join(baseline, 'dist/src/cli/pilotdeck.js'),
 baselineCLISource: join(baseline, 'src/cli/pilotdeck.ts'),
 baselineWebSocket: join(baseline, 'src/gateway/server/websocket.ts'),
 baselineWebSocketDist: join(baseline, 'dist/src/gateway/server/websocket.js'),
 baselineConnection: join(baseline, 'src/gateway/server/GatewayWsConnection.ts'),
 baselineConnectionDist: join(baseline, 'dist/src/gateway/server/GatewayWsConnection.js'),
 candidateHTTP: join(root, 'src/pilotdeck-gateway/src/http_proxy.rs'),
 candidateFrames: join(root, 'src/pilotdeck-gateway/src/frame.rs'),
 candidatePairing: join(root, 'src/pilotdeck-gateway/src/pairing.rs'),
};
const inputs = {baselineGitHead: execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {encoding:'utf8'}).trim(), nodeVersion: process.version, nodeExecutable: process.execPath,
 files: Object.fromEntries(await Promise.all(Object.entries(inputPaths).map(async ([key, path]) => [key, {path, sha256Before: await sha(path)}]))),
 protocol: {helloHex: hello.toString('hex'), initialPingHex: initial.toString('hex'),
  postRequestHex: frame(JSON.stringify({type:'request',id:'post',method:'describe_server'})).toString('hex'),
  fencePingHex: frame('FENCE',9).toString('hex'), suffixPrefixBytes: 3, token: 'probe-token', maskingKeyHex: '0b16212c'}};
await writeFile(join(out, 'inputs.json'), JSON.stringify(inputs, null, 2) + '\n');
let status='FAIL',error=null,results=[];
try{
 Object.assign(env,await provision(runtime));
 // Bind a private ephemeral port before launching, then release this owned reservation.
 const reserve=net.createServer();const tsPort=await listen(reserve);await new Promise(r=>reserve.close(r));servers.splice(servers.indexOf(reserve),1);
 env.PILOTDECK_GATEWAY_PORT=String(tsPort);
 const ts=await child('baseline',baselineArgv(),env);await ready(tsPort,ts);
 for(const side of ['ts','rust'])for(const [head,split,method] of [...['empty','full','partial'].flatMap(head=>[false,true].map(split=>[head,split,'GET'])),['empty',false,'POST']]){
  const id=method==='POST'?side+'-post-upgrade':side+'-'+head+'-'+(split?'split':'whole');let held=[],release=null,arrived;
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
  const request=Buffer.from(method+' /ws?probe='+id+' HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+nonce+'\r\nSec-WebSocket-Version: 13\r\n\r\n');
  await send(c,Buffer.concat([request,tail]));await deadline(barrier,'held real 101');
  if(split){await send(c,hello.subarray(0,3));await send(c,hello.subarray(3))}else await send(c,hello);
  rec({id,event:'later-client-write-complete-before-release',bytes:hello.length});release();
  await r.wait(()=>r.frames.some(f=>f.json?.type==='hello_ok'));
  await send(c,frame(JSON.stringify({type:'request',id:'post',method:'describe_server'})));
  await r.wait(()=>r.frames.some(f=>f.json?.id==='post'));
  await send(c,frame('FENCE',9));await r.wait(()=>r.frames.some(f=>f.op===10&&Buffer.from(f.payloadHex,'hex').toString()==='FENCE'));
  if(r.frames.length!==3||r.frames.filter(f=>f.json?.type==='hello_ok').length!==1||r.frames.filter(f=>f.json?.id==='post').length!==1||r.frames.filter(f=>f.op===10&&Buffer.from(f.payloadHex,'hex').toString()==='FENCE').length!==1||r.frames.some(f=>f.op===10&&f.payloadHex===Buffer.from('INITIAL').toString('hex')))throw Error('duplicate/lost or initial head processed '+id);
  rec({id,event:'validated'});results.push({id,side,method,requestHex:request.toString('hex'),rawWireHex:r.rawHex,initialTailHex:tail.toString('hex'),laterSplit:split,frames:r.frames,relayConnections:relayClients});c.destroy();await once(c,'close');await new Promise(r=>relay.close(r));servers.splice(servers.indexOf(relay),1);
  if(rs){process.kill(-rs.pid,'SIGTERM');await deadline(once(rs,'exit'),'Rust stop')}
 }

 // Ordinary /ws without Connection: Upgrade goes through the HTTP handler.
 for (const side of ['ts', 'rust']) {
  const id = side + '-get-no-token';
  let port = tsPort, rs = null;
  if (side === 'rust') {
   port = await availablePort();
   rs = await child(id, [binary], {...env, PILOTDECK_PROXY_LISTEN: '127.0.0.1:' + port,
    PILOTDECK_PROXY_UPSTREAM: 'http://127.0.0.1:' + tsPort});
   await ready(port, rs);
  }
  const c = await connect(port), chunks = [];
  c.on('data', chunk => chunks.push(Buffer.from(chunk)));
  const ended = once(c, 'end');
  const request = Buffer.from('GET /ws?probe=' + id + ' HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: close\r\n\r\n');
  await send(c, request);
  await deadline(ended, 'ordinary GET EOF');
  const wire = Buffer.concat(chunks), boundary = wire.indexOf('\r\n\r\n');
  if (boundary < 0) throw Error('incomplete ordinary HTTP response');
  routingResults.push({id, side, requestHex: request.toString('hex'), rawWireHex: wire.toString('hex'),
   statusLine: wire.subarray(0, wire.indexOf('\r\n')).toString(), bodyHex: wire.subarray(boundary + 4).toString('hex'), eof: c.readableEnded});
  c.destroy();
  if (rs) await stopChild(rs);
 }
 if (routingResults[0].statusLine !== routingResults[1].statusLine ||
     routingResults[0].bodyHex !== routingResults[1].bodyHex ||
     !routingResults.every(row => row.eof)) throw Error('ordinary /ws route mismatch');
 await stopChild(ts);

 // A parser exception can poison the baseline shutdown state. Each suffix
 // observation therefore gets a new process and a distinct config/database.
 for (const side of ['ts', 'rust']) {
  const id = side + '-partial-suffix', isolated = join(runtime, id);
  const isolatedEnv = await provision(isolated);
  const portTS = await availablePort();
  const baselineChild = await child(id + '-baseline', baselineArgv(),
   {...isolatedEnv, PILOTDECK_GATEWAY_PORT: String(portTS)}, join(isolated, 'project'));
  await ready(portTS, baselineChild);
  let port = portTS, rs = null;
  if (side === 'rust') {
   port = await availablePort();
   rs = await child(id, [binary], {...isolatedEnv, PILOTDECK_PROXY_LISTEN: '127.0.0.1:' + port,
    PILOTDECK_PROXY_UPSTREAM: 'http://127.0.0.1:' + portTS}, join(isolated, 'project'));
   await ready(port, rs);
  }
  const c = await connect(port), r = reader(c), socketErrors = [];
  c.on('error', error => socketErrors.push({code: error.code, message: error.message}));
  const nonce = randomBytes(16).toString('base64');
  const request = Buffer.from('GET /ws?probe=' + id + ' HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ' + nonce + '\r\n\r\n');
  const prefix = initial.subarray(0, 3), suffix = initial.subarray(3);
  await send(c, Buffer.concat([request, prefix]));
  await r.wait(() => r.header !== null);
  // Only the suffix follows 101: do not hide the parser failure behind hello.
  await send(c, suffix);
  const exception = await observation(row => row.pid === baselineChild.pid && row.event === 'uncaught-exception');
  const observations = await readObservations();
  const upgrade = observations.filter(row => row.event === 'upgrade' && row.url === '/ws?probe=' + id);
  const row = {id, side, baselinePid: baselineChild.pid, requestHex: request.toString('hex'),
   initialTailHex: prefix.toString('hex'), laterSuffixHex: suffix.toString('hex'),
   actualTsHeadHex: upgrade.length === 1 ? upgrade[0].headHex : null,
   actualTsDataHex: observations.filter(row => row.event === 'socket-data' && row.pid === baselineChild.pid && row.id === upgrade[0]?.id).map(row => row.hex).join(''),
   exception, rawWireHex: r.rawHex, after101WireHex: Buffer.from(r.rawHex, 'hex').subarray(Buffer.byteLength(r.header)).toString('hex'), frames: r.frames,
   socketBeforeCleanup: {readableEnded: c.readableEnded, destroyed: c.destroyed, closed: c.closed, errors: socketErrors},
   baselineBeforeCleanup: {exitCode: baselineChild.exitCode, signalCode: baselineChild.signalCode}};
  suffixResults.push(row);
  if (upgrade.length !== 1 || row.actualTsHeadHex !== (side === 'ts' ? prefix.toString('hex') : '') ||
      row.actualTsDataHex !== suffix.toString('hex')) throw Error('suffix HTTP/data ownership mismatch ' + id);
  // Snapshot EOF before active cleanup. The baseline may leave this socket
  // open while its exception handler waits for server.close().
  c.destroy();
  await stopChild(baselineChild);
  if (rs) await stopChild(rs);
 }
 checkSuffix(suffixResults);
 const observations=(await readFile(join(out,'ts-observed.jsonl'),'utf8')).trim().split('\n').map(JSON.parse);
 for(const result of results){const o=observations.filter(o=>o.event==='upgrade'&&o.url==='/ws?probe='+result.id);if(o.length!==1)throw Error('upgrade observation missing '+result.id);result.actualTsHeadHex=o[0].headHex;result.actualTsMethod=o[0].method;if(o[0].method!==result.method)throw Error('baseline method differs '+result.id);if(result.side==='ts'&&o[0].headHex!==result.initialTailHex)throw Error('TS read boundary did not match '+result.id);if(result.side==='rust'&&o[0].headBytes!==0)throw Error('Rust forwarded initial head '+result.id)}
 for (const row of routingResults) {
  const normal = observations.filter(event => event.event === 'http-request' && event.url === '/ws?probe=' + row.id);
  if (normal.length !== 1 || normal[0].method !== 'GET' || observations.some(event =>
   event.event === 'upgrade' && event.url === '/ws?probe=' + row.id)) throw Error('ordinary GET dispatched as upgrade');
  row.actualTsMethod = normal[0].method;
 }
 for (const rows of [results, suffixResults, routingResults]) {
  if (new Set(rows.map(row => row.id)).size !== rows.length) throw Error('duplicate case ID');
 }
 if(results.length!==14 || routingResults.length!==2 || suffixResults.length!==2)throw Error('incomplete matrix');status='PASS';
}catch(e){error=e.stack??String(e)}finally{
 for(const s of sockets)s.destroy();for(const s of servers)try{await deadline(new Promise(r=>s.close(()=>r())),'listener cleanup',3000,true)}catch(error){cleanupErrors.push(String(error))};
 for (const child of [...children].reverse()) {
  if (child.p.exitCode === null && child.p.signalCode === null) {
   try { process.kill(-child.p.pid, 'SIGTERM'); } catch {}
   try {
    await deadline(once(child.p, 'exit'), 'child stop', 3000, true);
   } catch {
    try { process.kill(-child.p.pid, 'SIGKILL'); } catch {}
    try { await deadline(once(child.p, 'exit'), 'child kill', 3000, true); }
    catch (error) { cleanupErrors.push(String(error)); }
   }
  }
  try {
   await deadline(child.closed, 'child stdio close', 3000, true);
   await deadline(new Promise((resolve, reject) => {
    child.log.once('error', reject);
    child.log.end(resolve);
   }), 'log flush', 3000, true);
  } catch (error) { cleanupErrors.push(String(error)); }
 }
 try{sourceIdentity.after=verifySourceIdentity(root,sourceBefore);sourceIdentity.verified=true;}
 catch(e){status='FAIL';sourceIdentity.error={code:e.code??'SOURCE_IDENTITY_ERROR',message:String(e),details:e.details??null};error??='source identity verification failed';}
 let binaryAfter=null,baselineSourceAfter=null,baselineDistAfter=null;
 try{binaryAfter=await sha(binary);baselineSourceAfter=await sha(join(baseline,'src/gateway/server/GatewayServer.ts'));baselineDistAfter=await sha(join(baseline,'dist/src/gateway/server/GatewayServer.js'));}
 catch(e){status='FAIL';cleanupErrors.push('final binary/baseline hash: '+String(e));error??='final hash unavailable';}
 if(binaryBefore!==binaryAfter||baselineSourceBefore!==baselineSourceAfter||baselineDistBefore!==baselineDistAfter){status='FAIL';error='binary or baseline changed during capture'}
 if(cleanupErrors.length||children.some(x=>x.p.exitCode===null&&x.p.signalCode===null)){status='FAIL';error??='cleanup incomplete'}
 for (const binding of configurationBindings) {
  try{
  binding.sha256After = await sha(binding.path);
  if (binding.sha256Before !== binding.sha256After) { status = 'FAIL'; error = 'runtime configuration changed'; }
  }catch(e){status='FAIL';binding.verificationError=String(e);error??='runtime configuration hash unavailable';}
 }
 for (const binding of Object.values(inputs.files)) {
  try{
  binding.sha256After = await sha(binding.path);
  if (binding.sha256Before !== binding.sha256After) { status = 'FAIL'; error = 'key input changed'; }
  }catch(e){status='FAIL';binding.verificationError=String(e);error??='key input hash unavailable';}
 }
 const artifactSHA256={};for(const name of ['inputs.json','ts-observed.jsonl',...children.map(child=>child.name+'.log')]){try{artifactSHA256[name]=await sha(join(out,name));}catch(e){status='FAIL';artifactSHA256[name]=null;cleanupErrors.push('artifact hash '+name+': '+String(e));}}
 const report={status,error,cleanupErrors,inputs,sourceIdentity,configurationBindings,artifactSHA256,suffixResults,routingResults,scope:'actual default baseline CLI/GatewayServer HTTP entry and exact candidate binary; initial Rust read ownership proved separately by scripted production-path unit tests',results,events,children:children.map(({name,p,argv})=>({name,pid:p.pid,argv,exitCode:p.exitCode,signalCode:p.signalCode,stopped:p.exitCode!==null||p.signalCode!==null})),candidateRoot:root,candidateHead,candidateBinary:binary,binarySha256Before:binaryBefore,binarySha256After:binaryAfter,baselineSourceSha256Before:baselineSourceBefore,baselineSourceSha256After:baselineSourceAfter,baselineDistSha256Before:baselineDistBefore,baselineDistSha256After:baselineDistAfter};
 await writeFile(join(out,'report.json'),JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({status,cases:results.length+suffixResults.length+routingResults.length,upgradeCases:results.length,suffixCases:suffixResults.length,routingCases:routingResults.length,error}));if(status!=='PASS')process.exitCode=1;
}
