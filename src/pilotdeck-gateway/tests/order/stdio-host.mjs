// Real default TS assembly over process pipes. stdout is exclusively wire bytes.
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {pathToFileURL} from 'node:url';
import {bind,sha} from './guard.mjs';
import {prepareFixture} from './fixture.mjs';
import {decode} from './protocol.mjs';
const spec=JSON.parse(await fs.readFile(process.argv[2],'utf8'));
assert.equal(process.version,'v22.22.0');
assert.equal(process.env.PDRS_ORDER_PROBE,'owned-default-six');
assert.equal(await fs.realpath(process.execPath),spec.node.path);
const scratch=await fs.realpath(spec.scratch);
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])assert.ok((await fs.realpath(process.env[key])).startsWith(scratch+path.sep),key);
const verify=async()=>{
  for(const tool of ['node','git','loader'])assert.equal(sha(await fs.readFile(spec[tool].path)),spec[tool].sha256,'tool drift '+tool);
  for(const [name,digest]of Object.entries(spec.scripts)){
    assert.equal(path.basename(name),name,'script manifest filename');
    assert.equal(sha(await fs.readFile(path.join(spec.scriptRoot,name))),digest,'script drift '+name);
  }
};
await verify();const before=bind(spec.baseline,spec.git.path);
const {project,pilot}=await prepareFixture(scratch);
const events=[];let index=0;const record=(event,extra={})=>events.push({eventIndex:++index,event,...extra});
const load=p=>import(pathToFileURL(path.join(spec.baseline,'src',p)).href);
const {createLocalGateway}=await load('cli/createLocalGateway.ts');
const {TextWebSocketConnection}=await load('gateway/server/websocket.ts');
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const local=createLocalGateway();let finishing=false;
// Watchdog terminates only failure. It never selects successful output order.
const watchdog=setTimeout(()=>{process.stderr.write('host watchdog\n');process.exit(2);},15000);
async function finish(failure) {
  if(finishing)return;finishing=true;let disposed=false,after;
  try{await local.dispose();disposed=true;await verify();after=bind(spec.baseline,spec.git.path);assert.deepEqual(after,before);}catch(error){failure??=String(error);}
  const report={status:failure?'FAIL':'RECORDED_NOT_ACCEPTED',failure,sourceBefore:before,sourceAfter:after,spec,node:process.version,project,pilot,events,cleanup:{disposed},scope:{transport:'stdio pipes + actual TextWebSocketConnection/GatewayWsConnection',osCompletion:'NOT_OBSERVED',methodReady:'NOT_OBSERVED',taskConsumable:'NOT_OBSERVED'}};
  await fs.writeFile(spec.report,JSON.stringify(report,null,2)+'\n');clearTimeout(watchdog);
  // EOF is a causal end marker; explicit exit also bounds default watcher cleanup.
  process.stdout.end(()=>process.exit(failure?1:0));
}
class Socket extends EventEmitter {
  write(raw){const bytes=Buffer.from(raw);record('wire_write',{wireHex:bytes.toString('hex'),...decode(bytes)});return process.stdout.write(bytes);}
  end(){this.emit('close');void finish();}
}
const socket=new Socket();
const ws=new TextWebSocketConnection(socket);
new GatewayWsConnection(ws,{gateway:local.gateway,token:'fixture',serverVersion:'owned-probe'});
process.stdin.on('data',chunk=>{record('input_data',{readBatch:events.filter(e=>e.event==='input_data').length+1,wireHex:chunk.toString('hex')});socket.emit('data',chunk);record('parser_callback_return');});
process.stdin.on('end',()=>{socket.emit('close');void finish();});
process.stdin.on('error',error=>void finish(String(error)));
process.on('uncaughtException',error=>void finish(String(error)));
process.on('unhandledRejection',error=>void finish(String(error)));
