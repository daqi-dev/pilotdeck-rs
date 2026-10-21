import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/server/GatewayWsConnection.ts', 'src/gateway/protocol/version.ts',
  'src/interaction/InteractionReconnect.ts', 'src/interaction/InteractionReplay.ts',
  'src/extension/skills/SkillManager.ts', 'src/gateway/dialog/errors.ts',
  'src/gateway/permission/GatewayPermissionBus.ts', 'src/gateway/elicitation/GatewayElicitationBus.ts',
  'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts', 'src/gateway/client/GatewayInteractionCoordinator.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const req = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(req.resolve('tsx'))('esbuild');
const uuids = new Map<string, string>();
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'), {loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const module = {exports:{} as any};
  new Function('require','module','exports',code)((name: string) => {
    if (name in imports) return imports[name];
    if (name === 'node:crypto') return {...req(name), randomUUID() {
      const id = req(name).randomUUID(); uuids.set(id, `@connection-${uuids.size + 1}`); return id;
    }};
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, {get(_t,p) { if (p === '__esModule') return true;
      throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`); }});
  },module,module.exports);
  return module.exports;
}
const replay = await load(sources[3]);
const { createNativeInteractionReconnectPort } = await load(sources[2], {'./InteractionReplay.js':replay});
const skills = await load(sources[4]);
const errors = await load(sources[5]);
const version = await load(sources[1]);
const { GatewayWsConnection } = await load(sources[0], {'../protocol/version.js':version,
  '../../extension/skills/index.js':skills, '../dialog/errors.js':errors});
const interaction = {createNativeInteractionReconnectPort};
const permission = await load(sources[6], {'../../interaction/index.js':interaction});
const elicitation = await load(sources[7], {'../../interaction/index.js':interaction});
const grants = await load(sources[8]);
const {GatewayInteractionCoordinator} = await load(sources[9], {'../../interaction/index.js':interaction,
  '../permission/GatewayPermissionBus.js':permission, '../elicitation/GatewayElicitationBus.js':elicitation,
  '../permission/GatewaySessionPermissionRuleSetRegistry.js':grants});
const hello = (clientName = 'ui', extra = {}) => ({type:'hello',protocolVersion:'1.2',clientName,clientVersion:'test',token:'fixture-token',...extra});
const message = (value: any) => ({op:'message',raw:JSON.stringify(value)});
const request = (method: string, params: any = {}, id = method) => message({type:'request',id,method,params});
const scenarios: any[] = [
  {name:'invalid-json',steps:[{op:'message',raw:'{'}]},
  {name:'hello-required',steps:[message({type:'request',id:'x',method:'describe_server'})]},
  {name:'hello-missing-client-version',steps:[message({type:'hello',protocolVersion:'wrong',clientName:'ui',token:'wrong'})]},
  {name:'hello-wrong-client-type',steps:[message(hello(42 as any))]},
  {name:'protocol-before-auth',steps:[message(hello('ui',{protocolVersion:'wrong',token:'wrong'}))]},
  {name:'wrong-token',steps:[message(hello('ui',{token:'wrong'}))]},
  {name:'unknown-client-name',steps:[{op:'notify'},message(hello('future-client')), {op:'notify'},message({type:'hello'}),{op:'message',raw:'{'}]},
  {name:'empty-client-name',steps:[message(hello('')),message({type:'request',id:7,method:'x'})]},
  {name:'request-errors',steps:[message(hello()),request('steer_turn'),request('cancel_steer'),request('skill_validate'),request('skill_read'),request('unknown_method')]},
];
scenarios.push({name:'hello-publishes-auth-before-describe',holdDescribe:true,steps:[{op:'start_hello',frame:hello('sdk')},request('permission_decide',{sessionKey:'s'}),{op:'finish_hello'}]});
const forged = {connectionId:'forged',generation:'900'};
for (const client of ['ui','sdk','future-client']) scenarios.push({name:`binding-${client}`,steps:[message(hello(client)),
  request('permission_decide',{sessionKey:'s',requestId:'control',interactionBinding:forged}),
  request('permission_decide',{interactionBinding:forged},'no-session'),
  request('elicitation_respond',{sessionKey:'s',interactionBinding:forged}),
  request('reconnect_interaction',{sessionKey:'s',nextBinding:forged,interactionBinding:forged}),
  request('permission_decide',{sessionKey:'s',requestId:'run',projectKey:'p',interactionBinding:forged}),
  request('submit_turn',{sessionKey:'s',projectKey:'p',interactionBinding:forged}),
  request('permission_decide',{sessionKey:'s',requestId:'after',interactionBinding:forged}),
  {op:'close'},
]});
for (const preserve of [false,true]) scenarios.push({name:`close-preserve-${preserve}`,preserve,steps:[message(hello('sdk')),
  {op:'start',frame:{type:'request',id:'running',method:'submit_turn',params:{sessionKey:'old-socket',runId:'old',interactionBinding:forged}}},
  {op:'close'}, {op:'finish'}]});
scenarios.push({name:'close-union-order',steps:[message(hello()),request('reconnect_interaction',{sessionKey:'interaction-first'}),
  {op:'start',frame:{type:'request',id:'running',method:'submit_turn',params:{sessionKey:'in-flight-first'}}},
  {op:'close'}, {op:'finish'}]});
scenarios.push({name:'same-session-concurrent-submit-first-finishes-before-close',steps:[message(hello('sdk')),
  {op:'start',slot:'first',frame:{type:'request',id:'first',method:'submit_turn',params:{sessionKey:'shared'}}},
  {op:'start',slot:'second',frame:{type:'request',id:'second',method:'submit_turn',params:{sessionKey:'shared'}}},
  {op:'finish',slot:'first'},
  request('permission_decide',{sessionKey:'shared',requestId:'still-run'}),
  {op:'close'}, {op:'finish',slot:'second'}]});
scenarios.push({name:'permission-decide-null-and-missing-params',steps:[message(hello()),
  request('permission_decide',null,'null-params'),
  message({type:'request',id:'missing-params',method:'permission_decide'}),
  request('permission_decide',{},'object-params')]});
// Runtime inputs deliberately cross the declared TS string type.
for (const sessionKey of [5, true, 0, false, null, '']) scenarios.push({
  name:`session-key-${JSON.stringify(sessionKey)}`, permissionEvent:true, steps:[message(hello('sdk')),
    {op:'start',frame:{type:'request',id:'running',method:'submit_turn',params:{sessionKey,projectKey:'p'}}},
    request('reconnect_interaction',{sessionKey}),
    request('permission_decide',{sessionKey,requestId:'control'}),
    {op:'close'}, {op:'finish'}]});
for (const submitMode of ['throw', 'completed-then-throw', 'empty', 'permission']) scenarios.push({
  name:`submit-${submitMode}`, submitMode, steps:[message(hello()), request('submit_turn',{sessionKey:'s',projectKey:'p'})]});
scenarios.push({name:'disconnect-failure-stops-close',failDisconnect:true,steps:[message(hello()),
  request('reconnect_interaction',{sessionKey:'s1'}),
  {op:'start',frame:{type:'request',id:'running',method:'submit_turn',params:{sessionKey:'s2'}}},
  {op:'close_error'}, {op:'close_error'}, {op:'finish'}]});
scenarios.push({name:'request-after-close',steps:[message(hello()),{op:'close'},request('permission_decide',{sessionKey:'s',requestId:'after-close'}),request('submit_turn',{sessionKey:'new-after-close'}),{op:'close'}]});
const snapshot = (value: any): any => JSON.parse(JSON.stringify(value, (_k,v) => typeof v === 'string' && uuids.has(v) ? uuids.get(v) : v));
for (const scenario of scenarios) {
  const frames: any[] = [], closes: any[] = [], calls: any[] = [], finalInFlight: any[] = [], closeErrors: any[] = [];
  const port = createNativeInteractionReconnectPort();
  const coordinator = new GatewayInteractionCoordinator({reconnectPort:port});
  const registrations: any[] = [];
  let messageHandler: any, closeHandler: any, startedDescribe: any, releaseDescribe: any;
  const submits = new Map<string, {ready(): void, wait: Promise<void>, release(): void, operation?: Promise<any>}>();
  let startingSlot: string | undefined;
  let held: Promise<any> | undefined;
  const text = {onMessage(h: any) {messageHandler=h;},onClose(h: any) {closeHandler=h;},
    sendText(s: string) {const frame=JSON.parse(s);frames.push(frame);if(frame.final===true)finalInFlight.push([...connection.inFlightSessions]);}, close(code: number, reason: string) {closes.push({code,reason});}};
  const gateway = {
    async describeServer() {calls.push({method:'describe_server'});if (scenario.holdDescribe) {startedDescribe();await new Promise<void>(r=>{releaseDescribe=r;});}return {name:'fixture-server',capabilities:['fixture']};},
    skillValidate: async () => {throw new skills.SkillValidationError({valid:false,issues:[{code:'fixture',message:'fixture validation'}]});},
    skillRead: async () => {throw new skills.SkillManagerError('not_found','fixture skill missing');},
    steerTurn: async () => {throw new Error('plain fixture error');},
    cancelSteer: async () => {throw Object.assign(new Error('structured fixture error'),{code:'FIXTURE_ERROR',details:{nested:{value:1}}});},
    permissionDecide: async (params: any) => {calls.push({method:'permission_decide',params});return {accepted:true};},
    respondElicitation: async (params: any) => {calls.push({method:'elicitation_respond',params});return {accepted:true};},
    reconnectInteraction: (params: any) => {calls.push({method:'reconnect_interaction',params});return coordinator.reconnect(params);},
    disconnectInteraction: async (params: any) => {calls.push({method:'disconnect',params});if(scenario.failDisconnect) throw new Error('fixture disconnect failed');return coordinator.disconnect(params);},
    abortTurn: async (params: any) => {calls.push({method:'abort',params});},
    async *submitTurn(params: any) {
      calls.push({method:'submit_turn',params});
      coordinator.reconnectForTurn(params.sessionKey,params.interactionBinding);
      if (scenario.preserve) registrations.push(coordinator.getPermissionBus().register(params.sessionKey,
        {requestId:'fixture-pending',toolCallId:'fixture-tool',toolName:'fixture',resolve() {},reject() {}}));
      if (scenario.permissionEvent || scenario.submitMode === 'permission') yield {type:'permission_request',requestId:'fixture-permission'};
      if (scenario.submitMode === 'throw') throw new Error('fixture submit failed');
      if (scenario.submitMode === 'empty') return;
      if (startingSlot !== undefined) {
        const gate = submits.get(startingSlot)!;
        gate.ready(); await gate.wait;
      }
      yield {type:'turn_completed',runId:'fixture-run'};
      if (scenario.submitMode === 'completed-then-throw') throw new Error('fixture submit failed');
    },
  };
  const connection = new GatewayWsConnection(text, {gateway,token:'fixture-token',serverVersion:'test'});
  // Retain the actual callbacks, but make their real async bodies awaitable.
  // The constructor callbacks already entered these exact methods; no method
  // body or validation/dispatch is replaced. Promise completion is explicit.
  const originalMessage = connection.handleMessage.bind(connection);
  const originalClose = connection.handleClose.bind(connection);
  let operation: Promise<any>;
  connection.handleMessage = (raw: string) => (operation = originalMessage(raw));
  connection.handleClose = () => (operation = originalClose());
  scenario.results=[];
  for (const step of scenario.steps) {
    if (step.op === 'message') {messageHandler(step.raw);await operation!;}
    else if (step.op === 'notify') connection.sendNotification('fixture',{value:1});
    else if (step.op === 'start_hello') {
      const ready = new Promise<void>(r=>{startedDescribe=r;});messageHandler(JSON.stringify(step.frame));held=operation!;await ready;
    } else if (step.op === 'finish_hello') {releaseDescribe();await held;}
    else if (step.op === 'start') {
      const slot = step.slot ?? 'default';
      let ready!: () => void, release!: () => void;
      const entered = new Promise<void>(r => {ready=r;});
      const wait = new Promise<void>(r => {release=r;});
      const gate = {ready,wait,release,operation:undefined as Promise<any> | undefined};
      assert.ok(!submits.has(slot)); submits.set(slot,gate); startingSlot=slot;
      messageHandler(JSON.stringify(step.frame)); gate.operation=operation!; await entered;
      startingSlot=undefined;
    } else if (step.op === 'close') {closeHandler(); await operation!;}
    else if (step.op === 'close_error') {closeHandler(); try {await operation!; assert.fail('expected close rejection');} catch(error) {closeErrors.push((error as Error).message);}}
    else if (step.op === 'finish') {
      const slot = step.slot ?? 'default'; const gate = submits.get(slot)!;
      gate.release(); await gate.operation; submits.delete(slot);
    }
    else throw new Error(step.op);
    scenario.results.push(snapshot({frames,closes,calls,finalInFlight,
      inFlight:[...connection.inFlightSessions],interactions:[...connection.interactionSessions],...(scenario.failDisconnect ? {closeErrors} : {})}));
  }
  assert.equal(submits.size,0);
  for (const registration of registrations) registration.dispose();
}
// Real provider, raw result shape. No pending consumer/producer is simulated.
const bindingSteps = [
  {op:'reconnect',next:{connectionId:'a',generation:1}},
  {op:'reconnect',next:{connectionId:'a',generation:1}},
  {op:'reconnect',next:{connectionId:'b',generation:2}},
  {op:'reconnect',next:{connectionId:'b',generation:1},previous:{connectionId:'a',generation:1}},
  {op:'reconnect',next:{connectionId:'b',generation:2},previous:{connectionId:'a',generation:1}},
  {op:'disconnect',binding:{connectionId:'a',generation:1}},
  {op:'current'}, {op:'disconnect',binding:{connectionId:'b',generation:2}},
  {op:'reconnect',next:{connectionId:'c',generation:3},previous:{connectionId:'b',generation:2}},
  {op:'reconnect',next:{connectionId:'c',generation:3}},
  ...['3',-1,0.5,9007199254740992,null].map(generation=>({op:'reconnect',next:{connectionId:'x',generation}})),
  {op:'reconnect',next:{connectionId:'',generation:4}},
  {op:'reconnect',next:{connectionId:'zero',generation:0}},
];
const port = createNativeInteractionReconnectPort();
const bindingResults = bindingSteps.map((step: any) => {
  try {return step.op === 'reconnect' ? port.reconnect('owner',step.next,step.previous)
    : step.op === 'disconnect' ? port.disconnect('owner',step.binding) : port.currentBinding('owner') ?? null;}
  catch(e) {return {error:(e as Error).message};}
});
assertBaseline(baseline); await baselineSourceHashes(baseline,sources);
await mkdir(resolve(process.argv[2]),{recursive:true});
await writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,scenarios,bindingSteps,bindingResults},null,2)+'\n');
console.log(`recorded ${scenarios.length} connection scenarios, ${bindingSteps.length} binding steps; ${uuids.size} actual UUIDs masked`);
