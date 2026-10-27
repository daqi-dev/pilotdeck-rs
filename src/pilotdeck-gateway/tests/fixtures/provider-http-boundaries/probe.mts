import http from 'node:http';
import { performance } from 'node:perf_hooks';
import { createRequire } from 'node:module';
import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0', '#411 calibrated Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
bindBaseline({baseline, commit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'});
const { streamModel, resolveStreamIdleTimeout } = await import(pathToFileURL(join(baseline, 'src/model/streaming/streamModel.ts')).href);
const { parseModelConfig } = await import(pathToFileURL(join(baseline, 'src/model/config/parseModelConfig.ts')).href);
const { installGlobalProxy, createLongTimeoutOptions, getGlobalProxyStateForTesting } = await import(pathToFileURL(join(baseline, 'src/cli/proxy.ts')).href);
const require = createRequire(join(baseline, 'package.json'));
const undici = require('undici');
const protocol = process.argv[2] ?? 'anthropic';
const idleArg = process.argv[3] === 'default' ? undefined : Number(process.argv[3] ?? 600000);
const bodyArg = process.argv[4] === 'default' ? undefined : Number(process.argv[4] ?? 600000);
const run = process.argv[5] ?? '1';
const start = performance.now();
let lastFrameAt: number | undefined;
let lastEventAt: number | undefined;
function serial(value: any, seen = new WeakSet()): any {
  if (!value || typeof value !== 'object') return value;
  if (seen.has(value)) return '[Circular]';
  seen.add(value);
  if (value instanceof Error) {
    const result: any = {name:value.name, message:value.message, cause: value.cause === undefined ? null : serial(value.cause, seen), stack:value.stack};
    for (const key of Object.getOwnPropertyNames(value)) if (!(key in result)) result[key] = serial(value[key], seen);
    return result;
  }
  if (Array.isArray(value)) return value.map(v => serial(v, seen));
  return Object.fromEntries(Object.entries(value).map(([k,v]) => [k,serial(v,seen)]));
}
function log(kind: string, value: any = {}) {
  const now = performance.now();
  console.log(JSON.stringify({kind,protocol,run,pid:process.pid,wall:new Date().toISOString(),elapsedMs:now-start,sinceServerFrameMs:lastFrameAt === undefined ? null : now-lastFrameAt,sinceLastCanonicalDataMs:lastEventAt === undefined ? null : now-lastEventAt,...serial(value)}));
}
// Remove proxy and optional debug/duration settings in this child only.
for (const key of ['PILOTDECK_PROXY','https_proxy','HTTPS_PROXY','http_proxy','HTTP_PROXY','ALL_PROXY','all_proxy','PILOTDECK_DUMP_REQUEST','LITELLM_MAX_STREAMING_DURATION_SECONDS']) delete process.env[key];
await installGlobalProxy();
if (bodyArg !== undefined) {
  const original = undici.getGlobalDispatcher();
  undici.setGlobalDispatcher(new undici.Agent({...createLongTimeoutOptions(), bodyTimeout:bodyArg}));
  await original.close();
}
const sockets = new Set<any>();
const server = http.createServer((req,res) => {
  log('server_request',{method:req.method,url:req.url,remoteAddress:req.socket.remoteAddress});
  req.resume();
  req.on('end', () => {
    res.writeHead(200,{'content-type':'text/event-stream','cache-control':'no-cache','connection':'keep-alive'});
    const payload = protocol === 'anthropic'
      ? {type:'content_block_delta',index:0,delta:{type:'text_delta',text:'probe-frame'}}
      : protocol === 'openai'
        ? {id:'probe',object:'chat.completion.chunk',choices:[{index:0,delta:{content:'probe-frame'},finish_reason:null}]}
        : {candidates:[{index:0,content:{role:'model',parts:[{text:'probe-frame'}]}}]};
    lastFrameAt = performance.now();
    res.write(`data: ${JSON.stringify(payload)}\n\n`, () => log('server_frame_flushed',{payload}));
    // Deliberately no heartbeat, no end, and no server-side timeout.
  });
});
server.requestTimeout = 0;
server.timeout = 0;
server.on('connection', socket => {sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
await new Promise<void>((resolve,reject) => {server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
const address = server.address() as any;
const config = parseModelConfig({providers:{p:{protocol,url:`http://127.0.0.1:${address.port}/v1`,apiKey:'local-probe-key',retry:{streamMaxRetries:0,...(idleArg === undefined ? {} : {streamIdleTimeoutMs:idleArg})},models:{m:{}}}}});
const request = {provider:'p',model:'m',messages:[{role:'user',content:[{type:'text',text:'hello'}]}]};
log('setup',{node:process.version,undici:require('undici/package.json').version,nodeBundledUndici:process.versions.undici,proxyState:getGlobalProxyStateForTesting(),baselineAgentOptions:createLongTimeoutOptions(),actualBodyTimeoutMs:bodyArg ?? createLongTimeoutOptions().bodyTimeout,resolvedIdleMs:resolveStreamIdleTimeout(config.providers.p),retry:config.providers.p.retry,origin:`http://127.0.0.1:${address.port}`,agentOverride:bodyArg !== undefined});
const hardStop = setTimeout(()=>{log('fatal_hard_stop');process.exit(3);},(idleArg ?? 600000)+20000);
try {
  for await (const event of streamModel(request as any,config)) {
    if (event.type !== 'error' && event.type !== 'request_started') lastEventAt = performance.now();
    log('canonical_event',{event});
  }
  log('stream_returned');
} catch (error) {
  log('stream_thrown',{error});
} finally {
  clearTimeout(hardStop);
  for (const socket of sockets) socket.destroy();
  await new Promise<void>(resolve => server.close(()=>resolve()));
  await undici.getGlobalDispatcher().destroy();
  log('cleanup_done');
}
