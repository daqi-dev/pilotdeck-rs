import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { mock } from 'node:test';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/permission/GatewayPermissionBus.ts',
  'src/gateway/elicitation/GatewayElicitationBus.ts', 'src/gateway/client/GatewayInteractionCoordinator.ts',
  'src/interaction/InteractionReconnect.ts', 'src/interaction/InteractionReplay.ts',
  'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts', 'src/permission/settings.ts'];
const sourceSha256 = await baselineSourceHashes(baseline,sources);
const req = createRequire(join(baseline,'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline,source),'utf8'),
    {loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const module = {exports:{} as any};
  new Function('require','module','exports',code)((name: string) => {
    if (name in imports) return imports[name];
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, {get(_t,p) {if(p==='__esModule')return true;
      throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`);}});
  },module,module.exports);
  return module.exports;
}
const replay = await load(sources[4]);
const interaction = await load(sources[3],{'./InteractionReplay.js':replay});
const permission = await load(sources[0],{'../../interaction/index.js':interaction});
const elicitation = await load(sources[1],{'../../interaction/index.js':interaction});
const grants = await load(sources[5]);
const settings = await load(sources[6]);
const {GatewayInteractionCoordinator} = await load(sources[2],{
  '../../interaction/index.js':interaction, '../../permission/index.js':settings,
  '../permission/GatewayPermissionBus.js':permission, '../elicitation/GatewayElicitationBus.js':elicitation,
  '../permission/GatewaySessionPermissionRuleSetRegistry.js':grants});
const b = (connectionId='a',generation=1) => ({connectionId,generation});
const reg = (kind: string,id='r',session='s',extra={}) => ({op:'register',kind,id,session,...extra});
const answer = (kind: string,id='r',session='s',extra={}) => kind==='question'
  ? {op:'answer',id,session,answer:{behavior:'answered',value:{text:'yes'}},...extra}
  : {op:'decide',id,session,decision:'allow',...extra};
const scenarios: any[] = [];
for (const kind of ['permission','question']) {
  scenarios.push({name:`${kind}-registration`,steps:[reg(kind),reg(kind),answer(kind,'unknown'),
    answer(kind,'r','z'),answer(kind),answer(kind),{op:'registration_dispose',tag:'r'},reg(kind,'r','s',{tag:'replacement'}),
    {op:'registration_dispose',tag:'r'},{op:'registration_dispose',tag:'replacement'},answer(kind)]});
  scenarios.push({name:`${kind}-reject-and-dispose`,steps:[reg(kind,'z'),reg(kind,'a'),reg(kind,'other','z'),
    {op:'reject',kind,session:'s',reason:'exact 中文\ud800'}, {op:'reject',kind,session:'s',reason:'again'},
    {op:'bus_dispose',kind},reg(kind),{op:'bus_dispose',kind,reason:'custom dispose'}]});
  scenarios.push({name:`${kind}-cancel-and-late-answer`,steps:[reg(kind),{op:'registration_dispose',tag:'r'},
    answer(kind),reg(kind,'r','s',{tag:'new'}),answer(kind)]});
  scenarios.push({name:`${kind}-unbound-consume`,steps:[reg(kind),{op:'consume',kind,session:'s',id:'r',value:{raw:true}},
    {op:'consume',kind,session:'s',id:'r',value:null}]});
  for (const first of ['response','timeout']) {
    scenarios.push({name:`${kind}-same-deadline-${first}-first`,steps:[reg(kind),
      ...[first,first==='response'?'timeout':'response'].map(event=>({op:'schedule',event,kind,session:'s',id:'r',delay:10})),
      {op:'tick',ms:10},answer(kind)]});
  }
  scenarios.push({name:`${kind}-timeout-before-reuse`,steps:[reg(kind),
    {op:'schedule',event:'timeout',kind,session:'s',id:'r',delay:10},answer(kind),reg(kind,'r','s',{tag:'new'}),
    {op:'tick',ms:10},answer(kind)]});
  scenarios.push({name:`${kind}-invalid-registration`,steps:[reg(kind,'','s'),reg(kind,'r',''),reg(kind,'\ud800','\ud800'),
    reg(kind,'\ud800','\ud800'),answer(kind,'\ud800','\ud800')]});
  scenarios.push({name:`${kind}-binding-lifecycle`,steps:[
    {op:'reconnect',session:'s',next:b()},reg(kind,'z','s',{payload:{text:'中文',nested:[1,null]}}),
    {op:'reconnect',session:'s',next:b('b',2)},answer(kind,'z','s',{binding:b('wrong',1)}),
    {op:'disconnect',session:'s',binding:b('wrong',1)}, {op:'disconnect',session:'s',binding:b()},
    answer(kind,'z','s',{binding:b()}),{op:'reconnect',session:'s',next:b('b',2)},
    {op:'reconnect',session:'s',next:b('b',2),previous:b()},answer(kind,'z','s',{binding:b()}),
    answer(kind,'z','s',{binding:b('b',2)}),{op:'disconnect',session:'s',binding:b('b',2)},
    {op:'reconnect',session:'s',next:b('new',0)}]});
}
scenarios.push({name:'permission-deny-session',steps:[reg('permission','z'),reg('permission','a'),reg('permission','other','z'),
  {op:'deny',session:'s',reason:'turn_aborted'}, {op:'deny',session:'s',reason:'again'},answer('permission','z'),
  {op:'bus_dispose',kind:'permission'}]});
scenarios.push({name:'shared-kind-and-coordinator-close',steps:[reg('permission'),reg('question'),
  {op:'grant',session:'s',entry:'Read'}, {op:'reconnect',session:'s',next:b()},
  {op:'disconnect',session:'s',binding:b()}, {op:'close',session:'s',reason:'session_closed'},
  answer('permission'),answer('question'),{op:'reconnect',session:'s',next:b('fresh',0)}]});
scenarios.push({name:'reject-turn-keeps-binding-and-rules',steps:[{op:'grant',session:'s',entry:'Bash( git:* )'},
  {op:'reconnect',session:'s',next:b()},reg('permission'),reg('question'),
  {op:'reject_turn',session:'s',reason:'turn_timeout'}, {op:'disconnect',session:'s',binding:b()},
  {op:'disconnect',session:'s',binding:b()}]});
scenarios.push({name:'coordinator-dispose-order-and-reuse',steps:[reg('permission','p','z'),reg('question','q','z'),
  reg('permission'),reg('question'),{op:'grant',session:'z',entry:'Write'}, {op:'reconnect',session:'s',next:b()},
  {op:'dispose',reason:'owner_exited'},{op:'dispose',reason:'again'},reg('permission'),answer('permission')]});
scenarios.push({name:'reconnect-for-turn',steps:[{op:'reconnect'},
  {op:'reconnect_turn',session:'s',next:b()},{op:'reconnect_turn',session:'s',next:b('b',2)},
  {op:'reconnect_turn',session:'s',next:b('a',2)},reg('question'),
  {op:'reconnect',session:'s',next:b('a',2)}, {op:'reconnect',session:'s',next:b('a',2),previous:b('bad',0)},
  {op:'reconnect',session:'s',next:b('other',2),previous:b('a',2)},
  {op:'disconnect',session:'s',binding:b('a',2)}, {op:'reject_turn',session:'s',reason:'cancelled'},
  {op:'reconnect',session:'s',next:b('b',0)}]});
scenarios.push({name:'pending-before-initial-binding-order-payload',steps:[reg('question','z','s',{payload:null}),
  reg('permission','a','s',{payload:{nested:{x:1},arr:[1,2]}}),{op:'reconnect',session:'s',next:b()},
  {op:'reconnect',session:'s',next:b()},answer('question','z','s',{binding:b()}),answer('permission','a','s',{binding:b(),remember:false,reason:'no reason'}),
  reg('permission','p'),answer('permission','p','s',{decision:'deny',remember:true,reason:'literal'})]});
for (const next of [{connectionId:'',generation:0},{connectionId:'x',generation:-1},
  {connectionId:'x',generation:0.5},{connectionId:'x',generation:'1'}, {connectionId:'x',generation:9007199254740992}]) {
  scenarios.push({name:`invalid-binding-${JSON.stringify(next)}`,steps:[{op:'reconnect',session:'s',next},
    {op:'disconnect',session:'s',binding:next}]});
}
scenarios.push({name:'grants-normalization-dedup-close',steps:[
  ...['',' \ufeff ','Read','Read','Bash( git:* )','bash:git:*','Bash( )','Bash(x\ny)',
    'Read:*.rs',':pattern','x:a:b','\ufeffWrite\u2029','\ud800: \udfff ',
    ...['Edit','NotebookEdit','MultiEdit','Glob','Grep','Bash','Task','TodoWrite','WebFetch','WebSearch']]
    .map(entry=>({op:'grant',session:'s',entry})),{op:'close',session:'s',reason:'close'}]});
for (const op of ['reject','deny']) scenarios.push({name:`${op}-callback-reentry`,steps:[
  reg(op==='deny'?'permission':'question','first','s',{reenter:true}),
  reg(op==='deny'?'permission':'question','last'), {op,kind:op==='deny'?'permission':'question',session:'s',reason:'cleanup'}]});
scenarios.push({name:'port-dispose-does-not-settle-bus',steps:[reg('permission'),reg('question'),
  {op:'port_dispose',session:'s'},answer('permission','r','s',{binding:b()}),answer('permission'),answer('question')]});

scenarios.push({name:'replay-boundary-values',steps:[reg('permission','special','s',{specialPayload:true}),
  {op:'reconnect',session:'s',next:b()},answer('permission','special','s',{binding:b()})]});
scenarios.push({name:'default-coordinator-ownership',defaultCoordinator:true,steps:[reg('question'),reg('permission'),
  {op:'reconnect_turn',session:'s',next:b()}, {op:'close',session:'s',reason:'default_close'}]});

const clone = (v: any) => JSON.parse(JSON.stringify(v));
for (const scenario of scenarios) {
  mock.timers.enable({apis:['setTimeout']});
  const port = interaction.createNativeInteractionReconnectPort();
  const suppliedP = new permission.GatewayPermissionBus(port), suppliedQ = new elicitation.GatewayElicitationBus(port);
  const events: any[] = []; const registrations = new Map<string,any>();
  const coordinator = new GatewayInteractionCoordinator({...(scenario.defaultCoordinator?{}:{reconnectPort:port,permissionBus:suppliedP,elicitationBus:suppliedQ}),
    onElicitationDelivered(sessionKey: string,requestId: string) {events.push({event:'delivered',session:sessionKey,id:requestId});}});
  const p = coordinator.getPermissionBus(), q = coordinator.getElicitationBus();
  const activePort = coordinator.getReconnectPort();
  const owners = [...new Set(['s',...scenario.steps.filter((s:any)=>typeof s.session==='string').map((s:any)=>s.session)])] as string[];
  const bus = (kind: string) => kind==='permission'?p:q;
  const register = (step: any) => {
    const {kind,id,session} = step;
    const callback = (event: string,value: any) => {
      events.push({event,kind,session,id,value,p:p.pendingCount(session),q:q.pendingCount(session),replay:activePort.snapshot(session).length});
      if(step.reenter)register(reg(kind,'reentered',session));
    };
    const entry = {requestId:id,toolCallId:step.toolCallId??`tc-${id}`,toolName:step.toolName??'fixture',
      resolve(value: any) {callback('resolve',value);},reject(e: Error) {callback('reject',e.message);}};
    registrations.set(step.tag??id,bus(kind).register(session,entry,
      step.specialPayload?{payload:{keep:'text',omit:undefined,nan:NaN,inf:Infinity,arr:[undefined,NaN],nested:JSON.parse('{\"__proto__\":{\"hidden\":1},\"visible\":2}')}}:'payload' in step?{payload:step.payload}:{}));
    return {registered:true};
  };
  function run(step: any): any {
    const session = step.session??'s';
    switch(step.op) {
      case 'register':return register(step);
      case 'registration_dispose':registrations.get(step.tag).dispose();return null;
      case 'answer':return coordinator.respondElicitation({sessionKey:session,requestId:step.id,answer:step.answer,interactionBinding:step.binding});
      case 'decide':return coordinator.decidePermission({sessionKey:session,requestId:step.id,decision:step.decision,
        remember:step.remember,reason:step.reason,interactionBinding:step.binding});
      case 'consume':{const entry=bus(step.kind).consume(session,step.id,step.binding);if(entry)entry.resolve(step.value);return {consumed:!!entry};}
      case 'reject':bus(step.kind).rejectSession(session,step.reason);return null;
      case 'deny':p.denySession(session,step.reason);return null;
      case 'bus_dispose':bus(step.kind).dispose(step.reason);return null;
      case 'reconnect':return coordinator.reconnect({sessionKey:session,nextBinding:step.next,previousBinding:step.previous});
      case 'reconnect_turn':return coordinator.reconnectForTurn(session,step.next);
      case 'disconnect':return coordinator.disconnect({sessionKey:session,binding:step.binding});
      case 'reject_turn':coordinator.rejectPendingTurn(session,step.reason);return null;
      case 'close':coordinator.closeSession(session,step.reason);return null;
      case 'dispose':coordinator.dispose(step.reason);return null;
      case 'port_dispose':activePort.disposeOwner(session);return null;
      case 'grant':return coordinator.grantSessionPermission({sessionKey:session,entry:step.entry});
      case 'schedule':{
        // A timer owns the exact registration, not a future reuse of its ID.
        const registration=registrations.get(step.id);
        setTimeout(()=>{
          if(step.event==='timeout') {
            if(!registration.active)return;
            const entry=bus(step.kind).consume(session,step.id);
            if(entry)entry.reject(new Error('request_timeout'));
          } else run(answer(step.kind,step.id,session));
        },step.delay);return null;
      }
      case 'tick':mock.timers.tick(step.ms);return null;
      default:throw new Error(`unknown op ${step.op}`);
    }
  }
  scenario.results=[];
  for(const step of scenario.steps) {
    let result:any; try {result=run(step);}catch(e){result={error:(e as Error).message};}
    scenario.results.push(clone({result,events,registrations:[...registrations].map(([tag,r])=>({tag,active:r.active})),
      owners:owners.map(session=>({session,p:p.pendingCount(session),q:q.pendingCount(session),
        hasP:p.hasPendingSession(session),hasQ:q.hasPendingSession(session),
        pendingP:p.hasPending(session,step.id??'r'),pendingQ:q.hasPending(session,step.id??'r'),
        pSnapshot:p.snapshot(session),qSnapshot:q.snapshot(session),snapshot:activePort.snapshot(session),
        binding:coordinator.getBinding(session)??null,rules:coordinator.sessionAllowRules(session)}))}));
  }
  mock.timers.reset(); coordinator.dispose('fixture_end');
}
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
const out = resolve(process.argv[2]);
assert.ok(!out.startsWith(baseline+'/'), 'never write into the baseline');
await mkdir(out,{recursive:true});
await writeFile(join(out,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,scenarios},null,2)+'\n',{flag:'wx'});
console.log(`recorded ${scenarios.length} scenarios, ${scenarios.reduce((n,s)=>n+s.steps.length,0)} steps`);
