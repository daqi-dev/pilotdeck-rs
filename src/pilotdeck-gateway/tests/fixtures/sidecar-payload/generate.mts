// Execute entire original modules, exposing private readers only in memory.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree} from '../../baseline_source_guard.mjs';
assert.equal(process.version, 'v22.22.0');
assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, sourceCommit, '/usr/bin/git');
const git = (...args: string[]) => execFileSync('/usr/bin/git', ['--no-replace-objects', '-C', baseline, ...args]);
assert.equal(git('rev-parse', 'HEAD').toString().trim(), sourceCommit);
assert.equal(git('status', '--porcelain', '--untracked-files=no').toString(), '');
const files = ['src/agent/modules/transport/agentLoopSidecarClient.ts',
  'src/agent/modules/protocol.ts', 'src/agent/modules/events/hostAgentEventBridge.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts', 'src/agent/protocol/events.ts'];
const sourceSha256 = Object.fromEntries(files.map(file => {
  const bytes = readFileSync(join(baseline, file));
  assert.ok(bytes.equals(git('show', `${sourceCommit}:${file}`)));
  return [file, createHash('sha256').update(bytes).digest('hex')];
}));
const require = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(require.resolve('tsx'))('esbuild');
const clientPath = join(baseline, files[0]);
const code = transformSync(readFileSync(clientPath, 'utf8') +
  '\nexport {SidecarTurnProtocol, readAgentEvent, readFinalTerminal};',
  {loader:'ts', format:'esm', target:'es2022', sourcefile:clientPath}).code.replace(
  /from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
    let dependency = resolve(dirname(clientPath), specifier);
    if (dependency.endsWith('.js') && existsSync(dependency.slice(0,-3)+'.ts')) dependency=dependency.slice(0,-3)+'.ts';
    return `from ${quote}${pathToFileURL(dependency).href}${quote}`;
  });
const {SidecarTurnProtocol, readAgentEvent, readFinalTerminal} = await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
const load = (file: string) => import(pathToFileURL(join(baseline,file)).href);
const {MODULE_PROTOCOL_VERSION} = await load(files[1]);
const {createHostAgentEventBridge} = await load(files[2]);
const {createSidecarDefaultModuleDispatcher} = await load(files[3]);
const {createAgentEventBuffer} = await load(files[4]);
const input = {sessionId:'session', turnId:'turn', messages:[], execution:{runId:'run', operationId:'operation'}};
const good = {type:'text_delta', sessionId:'session', turnId:'turn', text:'after'};
const terminal = {result:{type:'success', stopReason:'completed', sessionId:'session', turnId:'turn', usage:{}, permissionDenials:[], turns:1, startedAt:'fixed', completedAt:'fixed'}, messages:[]};
const binding = {protocolVersion:MODULE_PROTOCOL_VERSION, moduleId:'fixture', moduleInstanceId:'instance', connectionGeneration:'generation', capabilitiesVersion:'v1'};
const config = {cwd:'/fixture', provider:'fixture', model:'fixture', permissionMode:'default'};
let serial = 0;
const protocol = (handlers = {}) => new SidecarTurnProtocol({config, input, checkpoint:{snapshot:()=>undefined}, manifest:{tools:[]}, moduleHandlers:handlers, uuid:()=>String(++serial)});
const payloads = [
  ['missing', undefined], ['undefined', undefined], ['null', null], ['false', false],
  ['true', true], ['zero', 0], ['number', 42], ['empty-string', ''], ['string', 'event'], ['array', []], ['nonempty-array', [{}]], ['valid', good],
] as const;
const cases: any[] = [];
const observe = async (action: () => any) => {try { const value=await action(); return {ok:true, value}; } catch (error) {return {ok:false,error:(error as Error).message};}};
for (const [name, payload] of payloads) {
  const envelope = (final: boolean) => ({kind:'event', messageId:'event', eventType:'agent_event', streamId:'stream', ...input.execution,
    requestId:'request', sequence:0, final, ...(final?{outcome:'completed'}:{}), ...(name==='missing'?{}:{payload:name==='valid' && final?terminal:payload})});
  for (const final of [false, true]) {
    const p = protocol(), sent: any[] = [], yielded: any[] = [];
    let consumed = 0;
    const connection = {send:(message:any)=>{sent.push(message);}, async *receive() {
      yield {kind:'response', messageId:'response', inReplyTo:sent.at(-1).messageId, ok:true, ...binding};
      yield {kind:'response', messageId:'response', inReplyTo:sent.at(-1).messageId, ok:true, ...binding,
        payload:{capabilitiesVersion:'v1', methods:[{name:'execute', profiles:['streaming']}]}};
      const request = sent.at(-1);
      yield {kind:'response', messageId:'response', inReplyTo:request.messageId, requestId:request.requestId, ok:true, streamId:'stream'};
      consumed++; yield {...envelope(final), requestId:request.requestId};
      consumed++; yield {...envelope(false), requestId:request.requestId, sequence:1, payload:good};
      consumed++; yield {...envelope(true), requestId:request.requestId, sequence:2, payload:terminal};
    }};
    const result=await observe(async()=>{for await (const event of p.execute(connection)) yielded.push(event.type); return 'completed';});
    assert.equal(consumed, name==='valid' && !final?3:1, JSON.stringify(result));
    assert.equal(result.ok, name==='valid', JSON.stringify(result));
    cases.push({name, route:final?'wire-final':'wire-event', ...result, consumed, yielded});
  }
  cases.push({name, route:'embedded-reader', ...await observe(()=>readAgentEvent(envelope(false),input))});
  cases.push({name, route:'terminal-reader', ...await observe(()=>readFinalTerminal(envelope(true),input))});
  const aborted = new AbortController(); aborted.abort('compaction_persistence_failed');
  cases.push({name, route:'terminal-cancelled', ...await observe(()=>readFinalTerminal({...envelope(true),outcome:'cancelled',...(name==='valid'?{payload:{...terminal,result:{...terminal.result,type:'aborted'}}}:{})}, {...input,abortSignal:aborted.signal}).result.type)});
  const buffer=createAgentEventBuffer();
  const context={permissionContext:()=>({}),toolRuntimeContext:()=>({}),executionContext:()=>({})};
  const dispatcher=createSidecarDefaultModuleDispatcher({config,input,checkpoint:{},capabilityResultObserver:{},planTodoHandler:async()=>({}), modules:{
    model:{execution:{}}, capability:{runtimeContext:{bindTurn:()=>context},execution:{list:()=>[]}},
    permission:{requestContext:{bindTurn:()=>context}}, context:{requestIdentity:{bindTurn:()=>({contextIdentity:()=>({})})}},
    event:{emit:buffer.emitter,drain:buffer.drain}}});
  const p=protocol(dispatcher.handlers), replies:any[]=[];
  const bridge=createHostAgentEventBridge(async (call:any)=>{
    const reply=await p.moduleResponseFor({kind:'request',method:'module_call',messageId:String(++serial),...call});
    replies.push({ok:reply.response.ok, ...(reply.response.error?{error:reply.response.error.message}:{})});
    return reply.response;
  },input.execution,()=>String(++serial));
  bridge.emitter(payload); bridge.emitter(good);
  await bridge.flush();
  cases.push({name,route:'host-bridge',replies,drained:buffer.drain().map((e:any)=>e.type)});
  await dispatcher.dispose();
}
for (const row of cases) {
  let [, payload]: [string, any] = [...payloads.find(([name])=>name===row.name)!];
  if (row.name === 'valid' && ['wire-final','terminal-reader','terminal-cancelled'].includes(row.route)) {
    payload = row.route === 'terminal-cancelled' ? {...terminal,result:{...terminal.result,type:'aborted'}} : terminal;
  }
  if (row.name !== 'missing') row.payload = payload === undefined ? {__pdrsUndefined:true} : payload;
  console.log(JSON.stringify(row));
}
const output=resolve(process.argv[2]); mkdirSync(output,{recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify({sourceCommit,sourceSha256,cases})+'\n',{flag:'wx'});
console.log(`SIDECAR_PAYLOAD_PROBE_PASS node=${process.version} cases=${cases.length}`);
