// #748 real TS Gateway → AgentSession → TurnRunner → TS sidecar → native HTTP model / ToolRuntime.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readFileSync, writeFileSync, renameSync, mkdirSync, existsSync} from 'node:fs';
import {join, dirname, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
import {assertBaseline} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#748 driver pinned Node');
assert.equal(process.env.LIVE_E2E_MITM, undefined, '#748 driver rejects MITM override');
assert.ok(process.env.PDRS_BASELINE, '#748 driver requires PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
assertBaseline(baseline);
const fixture = dirname(fileURLToPath(import.meta.url));
const [scenario, outputArg] = process.argv.slice(2);
assert.ok(['text','tool','cancel-stream','cancel-tool','drop','duplicate','early-final'].includes(scenario), '#748 driver scenario');
assert.ok(outputArg, '#748 driver case directory');
const output = resolve(outputArg), sandbox = join(output, 'sandbox'), home = join(output, 'home');
assert.equal(process.env.HOME, home, '#748 driver HOME must be isolated');
mkdirSync(sandbox, {recursive:true}); mkdirSync(home, {recursive:true});
const write = (name: string, value: any) => writeFileSync(join(output, name), JSON.stringify(value, null, 2) + '\n', {flag:'wx'});
const publish = (name: string, value: any) => { write(name + '.tmp', value); renameSync(join(output,name+'.tmp'),join(output,name)); };
const load = (path: string) => import(pathToFileURL(join(baseline,'src',path)).href);
const [{InProcessGateway},{SessionRouter},{AgentSession},{TurnRunner},{JsonlTranscriptWriter},{FileRunRegistry},
 {createAgentLoopSidecarRuntimeFactory},{createStdioAgentLoopSidecarConnectionFactory},{createSidecarModuleComposition},
 {ToolRegistry},{ToolRuntime},{ConcurrentToolScheduler},{createToolSchedulerPort},{createReadFileTool},{createNodeFsPort},
 {createDefaultPermissionContext},{streamModel},{OPENAI_DEFAULT_CAPABILITIES},{DefaultContextRuntime},{createAgentTurnContextPort},
 {AgentSessionEventRecorder},{createDurableModelInvokerPort}] = await Promise.all([
 'gateway/client/InProcessGateway.ts','gateway/SessionRouter.ts','agent/session/AgentSession.ts','agent/turn/TurnRunner.ts',
 'session/transcript/JsonlTranscriptWriter.ts','gateway/run/RunRegistry.ts',
 'agent/modules/transport/agentLoopSidecarClient.ts','agent/modules/transport/stdioAgentLoopSidecarConnection.ts','agent/modules/transport/sidecarHostModulePorts.ts',
 'tool/registry/ToolRegistry.ts','tool/execution/ToolRuntime.ts','tool/scheduler/ConcurrentToolScheduler.ts','agent/modules/capability/toolSchedulerAdapter.ts',
 'tool/builtin/readFile.ts','tool/execution-world/NodeFsPort.ts','permission/index.ts','model/streaming/streamModel.ts','model/providers/openai/defaults.ts','context/DefaultContextRuntime.ts','agent/loop/AgentTurnCapabilities.ts',
 'agent/session/AgentSessionEventRecorder.ts','agent/modules/llm/durableModelInvokerPort.ts',
].map(load));
const injection = ['drop','duplicate','early-final'].includes(scenario) ? scenario : 'passthrough';
if (injection !== 'passthrough') write('plan.json', {injections:[{action:injection === 'early-final' ? 'early_final' : injection, direction:'s2h',kind:'event',final:false,nth:injection === 'duplicate' ? 1 : 2}]});
const mock = spawn(process.execPath,[join(fixture,'mock_provider.mjs')], {env:{...process.env,LIVE_E2E_MOCK_PORT:'0',LIVE_E2E_MOCK_LOG:join(output,'mock.jsonl'),LIVE_E2E_MOCK_READY:join(output,'mock-ready.json'),LIVE_E2E_INJECTION:injection}, stdio:['ignore','pipe','pipe']});
let mockStderr = ''; mock.stderr.on('data', chunk => { mockStderr += chunk; });
const deadline = setTimeout(() => { console.error('#748 driver watchdog'); mock.kill('SIGKILL'); process.exit(1); }, 60000);
const now = () => new Date('2026-10-26T00:00:00.000Z');
const sessionId = 'live-e2e-session', runId = `run-${scenario}`;
const target = join(sandbox,'tool-target.txt'); writeFileSync(target,'live-e2e read tool payload\n',{flag:'wx'});
let gateway: any, router: any, writer: any;
const rawEvents: any[] = [], modelEvents: any[] = [], toolResults: any[] = [];
let abortTask: Promise<any> | undefined, toolEntered = false, modelCalls = 0;
const abort = () => { if (!abortTask) abortTask = gateway.abortTurn({sessionKey:sessionId,runId,reason:'live_e2e_cancel'}); };
try {
  for (let attempt=0; !existsSync(join(output,'mock-ready.json')); attempt++) {
    assert.ok(attempt < 500 && mock.exitCode === null, `#748 mock startup: ${mockStderr}`); await delay(10);
  }
  const port = JSON.parse(readFileSync(join(output,'mock-ready.json'),'utf8')).port;
  const url = `http://127.0.0.1:${port}/v1`;
  const transcriptPath = join(output,'transcript.jsonl');
  let entry = 0, operation = 0;
  writer = new JsonlTranscriptWriter({path:transcriptPath,now,uuid:() => `entry-${++entry}`});
  const eventRecorder = new AgentSessionEventRecorder(writer,{uuid:() => `operation-${++operation}`});
  const modelConfig = {providers:{'live-e2e':{id:'live-e2e',protocol:'openai',url,apiKey:'mock-only',headers:{},retry:{streamMaxRetries:0},models:{'mock-model':{id:'mock-model',capabilities:{...OPENAI_DEFAULT_CAPABILITIES,supportsToolUse:true},multimodal:{input:['text']}}}}}};
  const modelPort = {async prepare({request}: any) {return {request,provider:request.provider,model:request.model};},
    async *stream({prepared,context}: any) { modelCalls++; for await (const event of streamModel(prepared.request,modelConfig as any,{signal:context.abortSignal})) {modelEvents.push(event); yield event;} }};
  const native = createNodeFsPort();
  const fs = {...native, async readFileInRange(...args: any[]) {
    if (scenario === 'cancel-tool') {
      toolEntered = true; publish('tool-entered.json',{entered:true});
      const signal = args[3] as AbortSignal;
      const aborted = new Promise<void>((done,reject) => {
        const timer = setTimeout(() => reject(Error('#748 tool gate abort deadline')),5000);
        const finish = () => {clearTimeout(timer);done();};
        if (signal.aborted) finish(); else signal.addEventListener('abort',finish,{once:true});
      });
      abort(); await aborted; publish('tool-release',{released:true});
    }
    return (native.readFileInRange as any)(...args);
  }};
  const registry = new ToolRegistry(); registry.register(createReadFileTool({fs}));
  const permission = {async decide() {return {type:'allow',reason:{type:'runtime',message:'fixture'}};}};
  const runtime = new ToolRuntime(registry,permission as any);
  const scheduler = new ConcurrentToolScheduler(runtime,registry);
  const toolPort = createToolSchedulerPort(registry,scheduler);
  const toolExecution = {...toolPort, async executeAll(...args:any[]) {const results=await (toolPort.executeAll as any)(...args); toolResults.push(...results); return results;}};
  const context = new DefaultContextRuntime({maxContextTokens:128000,projectRoot:sandbox,now});
  const metadata = {getModelMaxContextTokens:()=>128000,getModelMaxOutputTokens:()=>32768,getModelProtocol:()=> 'openai',getModelSupportsPromptCache:()=>false};
  const model = createDurableModelInvokerPort(modelPort,eventRecorder,sessionId);
  const modules = createSidecarModuleComposition({model,toolExecution,context:createAgentTurnContextPort(context),metadata,clock:{now}} as any);
  let serial = 0;
  const connect = createStdioAgentLoopSidecarConnectionFactory({command:process.env.LIVE_E2E_PYTHON ?? 'python3',args:[join(fixture,'stdio_mitm.py'),'--log',join(output,'frames.jsonl'),'--summary',join(output,'mitm-summary.json'),...(injection === 'passthrough' ? [] : ['--plan',join(output,'plan.json')]),'--',process.execPath,'--import',join(fixture,'determinism.mjs'),'--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),join(baseline,'src/cli/pilotdeck-agent-loop-sidecar.ts')],cwd:sandbox,env:{PATH:process.env.PATH!,HOME:home},killTimeoutMs:4000});
  const loop = createAgentLoopSidecarRuntimeFactory({connect,uuid:() => `fixed-${++serial}`})({config:{provider:'live-e2e',model:'mock-model',cwd:sandbox,permissionMode:'default',permissionContext:createDefaultPermissionContext({cwd:sandbox,mode:'default',canPrompt:false})},capabilities:{transport:{}} as any,sidecarModules:modules} as any);
  const runner = new TurnRunner(loop,writer,undefined,now,undefined,{cwd:sandbox,transcriptPath,collectFileArtifacts:false},{autoGenerateSessionTitle:false,eventRecorder});
  const session = new AgentSession({sessionId,turnRunner:runner,cwd:sandbox,transcriptPath,uuid:() => runId,eventRecorder});
  router = new SessionRouter({createSession:() => session,idleSweepIntervalMs:0,now});
  const runs = new FileRunRegistry(join(output,'registry.json'));
  gateway = new InProcessGateway(router,{runRegistry:runs,now,uuid:() => runId,abortTurnTimeoutMs:5000});
  const prompt = scenario === 'tool' ? `LIVE_E2E_TOOL ${target}` : scenario === 'cancel-tool' ? `LIVE_E2E_CANCEL_TOOL ${target}` : scenario === 'cancel-stream' ? 'LIVE_E2E_HANG' : 'LIVE_E2E_TEXT';
  for await (const event of gateway.submitTurn({sessionKey:sessionId,channelKey:'test',message:prompt,runId})) {
    // Only the Rust test feature can request this non-golden client projection attack.
    if (process.env.LIVE_E2E_MUTANT === 'client-payload' && event.type === 'assistant_text_delta') event.text = 'corrupted client projection';
    rawEvents.push(event);
    if (scenario === 'duplicate' && event.type === 'model_request_started') publish('event-visible.json',{type:event.type});
    if (scenario === 'cancel-stream' && event.type === 'assistant_text_delta') abort();
  }
  if (abortTask) await abortTask;
  const record = await runs.get({sessionKey:sessionId,runId});
  const registryEvents = await runs.events({sessionKey:sessionId,runId});
  const active = router.hasActiveTurn(sessionId);
  await router.shutdown(); router=undefined; await writer.close(); writer=undefined;
  const transcript = readFileSync(transcriptPath,'utf8').trim().split('\n').map(line => JSON.parse(line));
  write('raw-result.json',{scenario,events:rawEvents,transcript,record,registryEvents,active,modelCalls,modelEvents,toolResults,toolEntered});
  const canonical = (value:any):any => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).sort(([a],[b])=>a.localeCompare(b)).map(([key,item])=>[key,key === 'ownerPid' ? '<PID>' : canonical(item)]));
    if (typeof value === 'string') return value.split(output).join('<CASE>').split(`127.0.0.1:${port}`).join('127.0.0.1:<PORT>');
    return value;
  };
  const result = canonical({scenario,events:rawEvents,transcript,record,registryEvents,active,modelCalls,toolResults,toolEntered});
  const requests = new Map<number,any>();
  for (const [index,entry] of transcript.entries()) {
    if (entry.type === 'model_request') requests.set(entry.step,entry.request);
    if (entry.type === 'model_stream_event' && entry.event.type === 'request_started') {
      const request = requests.get(entry.step);
      assert.ok(request, '#748 request fingerprint has a durable request');
      const fingerprint = createHash('sha256').update(JSON.stringify({...request,stream:true})).digest('hex');
      assert.equal(entry.event.requestFingerprint,fingerprint,'#748 original request fingerprint');
      // The full canonical request remains in the preceding durable entry. Its
      // verified original fingerprint contains this capture's exclusive path.
      result.transcript[index].event.requestFingerprint = '<REQUEST-SHA256-VERIFIED>';
    }
  }
  write('result.json',result);
  console.log(`#748 ${scenario}: ${rawEvents.map(e=>e.type).join(',')} state=${record?.state}`);
} finally {
  if (router) await router.shutdown(); if (writer) await writer.close();
  mock.kill('SIGTERM');
  if (mock.exitCode === null) {
    const exited = new Promise(resolve=>mock.once('exit',resolve));
    const killTimer=setTimeout(()=>mock.kill('SIGKILL'),2000);
    await exited; clearTimeout(killTimer);
  }
  clearTimeout(deadline);
  writeFileSync(join(output,'mock-stderr.log'),mockStderr,{flag:'wx'});
}
