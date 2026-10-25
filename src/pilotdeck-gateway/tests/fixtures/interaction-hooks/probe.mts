// PDRS_BASELINE=<cc7c4e6 checkout> node --import <baseline>/node_modules/tsx/dist/loader.mjs probe.mts
// stdout is the oracle; this script never modifies the baseline or uses sockets.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
assert.ok(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, '/usr/bin/git');
const sources = [
  'src/extension/hooks/execution/HookRuntime.ts',
  'src/extension/hooks/execution/AsyncHookRegistry.ts',
  'src/lifecycle/runtime/LifecycleRuntime.ts',
  'src/gateway/permission/createGatewayPermissionHook.ts',
  'src/gateway/permission/GatewayPermissionBus.ts',
];
const [hr, ar, lr, ph, pb] = await Promise.all(sources.map(p => import(pathToFileURL(join(baseline, p)).href)));
const cases = [];
const suggestions = [null, { id:'allow_session', rules:'bad' }, { id:'allow_session', rules:[
  null, [], {toolName:'bash', pattern:'git *', behavior:'deny', source:'project'},
  {toolName:'', pattern:''}, {pattern:4}, {toolName:'bash', pattern:'git *'},
]}, { id:'allow_session', rules:[{toolName:'ignored'}] }];
for (const mode of ['allow', 'no-sink', 'deny']) {
  const hooks = new hr.HookRuntime({ PermissionRequest:[
    {hooks:[{type:'command',command:`printf '%s' '{"systemMessage":"configured"}'`}]},
    {hooks:[{type:'callback',name:'pilotdeck.gateway.permission'}]},
  ]});
  const rules = []; const bus = new pb.GatewayPermissionBus();
  hooks.getCallbackExecutor().register('pilotdeck.gateway.permission', ph.createGatewayPermissionHook({
    sessionKey:'s', permissionRules:rules, bus, uuid:()=> 'request', emit:event=> {
      if (mode === 'no-sink') return false;
      bus.consume('s',event.requestId)?.resolve({requestId:event.requestId,
        decision:mode==='deny'?'deny':'allow',reason:'blocked',remember:true});
      return true;
    },
  }));
  const runtime = new lr.LifecycleRuntime(hooks);
  const result = await runtime.dispatch({event:'PermissionRequest',
    baseInput:{sessionId:'s',transcriptPath:'',cwd:process.cwd()},
    payload:{toolName:'danger',toolUseId:'fallback-call',input:{arg:1},permissionSuggestions:suggestions},
    matchQuery:'PermissionRequest', env:{PATH:'/usr/bin:/bin'}});
  cases.push({mode,result,rules});
  await runtime.dispose();
}
const hooks = new hr.HookRuntime({PreToolUse:[{hooks:[{type:'command',command:`printf '%s' '{"async":true}'`,asyncRewake:true}]}]});
const runtime = new lr.LifecycleRuntime(hooks);
const pending = await runtime.dispatch({event:'PreToolUse',baseInput:{sessionId:'s',transcriptPath:'',cwd:process.cwd()},payload:{},matchQuery:'PreToolUse',env:{PATH:'/usr/bin:/bin'}});
pending.pendingAsyncHooks.forEach(d=>d.startedAt='<time>');
const id = pending.pendingAsyncHooks[0].id;
const completion = {stdout:'{"continue":false,"stopReason":"stop"}'};
const accepted = runtime.completeAsyncHook(id,completion);
const duplicate = runtime.completeAsyncHook(id,completion);
const responses = runtime.collectAsyncHookResponses();
const repeated = runtime.collectAsyncHookResponses();
await runtime.dispose();
// Baseline ID-only completion can affect a replacement registration; this is a
// difference observation, not the Rust expected result or an accepted waiver.
const registry = new ar.AsyncHookRegistry();
const item = {id:'same',hookName:'command',hookEvent:'PreToolUse',startedAt:'<time>',stdout:'',stderr:''};
const old = registry.register(item); const replacement = registry.register(item);
const staleIdAccepted = registry.complete(old.id,{stdout:'{}'});
await registry.dispose();
console.log(JSON.stringify({commit,sourceSha256:Object.fromEntries(sources.map(p=>[p,createHash('sha256').update(readFileSync(join(baseline,p))).digest('hex')])),suggestions,cases,async:{pending,accepted,duplicate,responses,repeated},difference:{oldGeneration:old.generation,newGeneration:replacement.generation,staleIdAccepted}},null,2));
