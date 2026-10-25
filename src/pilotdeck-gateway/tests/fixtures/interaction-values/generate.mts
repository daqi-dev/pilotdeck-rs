import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/interaction/InteractionReplay.ts', 'src/interaction/InteractionReconnect.ts',
  'src/gateway/permission/GatewayPermissionBus.ts', 'src/gateway/elicitation/GatewayElicitationBus.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { createNativeInteractionReconnectPort } = await import(pathToFileURL(join(baseline, sources[1])).href);
const { GatewayPermissionBus } = await import(pathToFileURL(join(baseline, sources[2])).href);
const { GatewayElicitationBus } = await import(pathToFileURL(join(baseline, sources[3])).href);
type Node = { tag: string, value?: any, fields?: [string, number][], items?: number[] };
const atom = (tag: string, value?: any): Node => ({ tag, ...(value === undefined ? {} : { value }) });
const seeds: [string, Node[]][] = [
  ['date', [atom('date', 1735787045678)]], ['invalid-date', [atom('date', 'NaN')]],
  ['date-upper', [atom('date', 8640000000000000)]], ['date-lower', [atom('date', -8640000000000000)]],
  ['date-outside', [atom('date', 8640000000000001)]], ['date-fraction', [atom('date', -1.9)]],
  ['bigint', [atom('bigint', '123456789012345678901234567890')]],
  ['negative-bigint', [atom('bigint', '-123456789012345678901234567890')]],
  ['zero-bigint', [atom('bigint', '0')]], ['function', [atom('function')]],
  ['symbol', [atom('symbol')]], ['undefined', [atom('undefined')]],
  ['nan', [atom('number', 'NaN')]], ['infinity', [atom('number', 'Infinity')]],
  ['negative-infinity', [atom('number', '-Infinity')]], ['negative-zero', [atom('number', '-0')]],
  ['null', [atom('json', null)]], ['boolean', [atom('json', false)]],
  ['string', [atom('json', '中文\ud800')]], ['finite', [atom('number', 42.5)]],
  ['cycle', [{tag:'object',fields:[['self',0]]}]], ['array-cycle', [{tag:'array',items:[0]}]],
  ['shared', [{tag:'object',fields:[['a',1],['b',1]]},{tag:'object',fields:[['x',2]]},atom('json',1)]],
  ['shared-json-object', [{tag:'array',items:[1,1]},atom('json',{x:1})]],
  ['shared-json-array', [{tag:'array',items:[1,1]},atom('json',[1])]],
  ['shared-date', [{tag:'array',items:[1,1]},atom('date',0)]],
  ['key-order', [{tag:'object',fields:[['b',1],['2',1],['1',1]]},{tag:'object',fields:[]}]],
  ['proto-alias', [{tag:'object',fields:[['__proto__',1],['x',1]]},{tag:'object',fields:[]}]],
  ['proto-invalid', [{tag:'object',fields:[['__proto__',1]]},atom('date','NaN')]],
  ['map', [{tag:'map',fields:[]}]], ['set', [{tag:'set',fields:[]}]],
  ['map-fields', [{tag:'map',fields:[['own',1]]},atom('bigint','7')]],
  ['set-fields', [{tag:'set',fields:[['own',1]]},atom('function')]],
  ['class', [{tag:'class',fields:[['own',1]]},atom('json','field')]],
  ['toJSON', [{tag:'object',fields:[['toJSON',1],['own',2]]},atom('function'),atom('json','field')]],
];
function construct(nodes: Node[], root: number): unknown {
  class Instance { inherited() { throw new Error('must not call prototype'); } }
  const values = nodes.map(n => n.tag === 'array' ? [] : n.tag === 'map' ? new Map([['hidden', 1n]])
    : n.tag === 'set' ? new Set([1n]) : n.tag === 'class' ? new Instance() : n.tag === 'object' ? {}
    : n.tag === 'date' ? new Date(Number(n.value)) : n.tag === 'bigint' ? BigInt(n.value)
    : n.tag === 'function' ? () => { throw new Error('must not call toJSON/function'); }
    : n.tag === 'symbol' ? Symbol('hidden') : n.tag === 'undefined' ? undefined
    : n.tag === 'number' ? Number(n.value) : n.value);
  nodes.forEach((n,i) => {
    if (n.items) for (const ref of n.items) (values[i] as unknown[]).push(values[ref]);
    for (const [key,ref] of n.fields ?? []) Object.defineProperty(values[i],key,
      {value:values[ref],enumerable:true,writable:true,configurable:true});
  });
  return values[root];
}
function observe(value: any): any {
  if (typeof value === 'number' && Object.is(value,-0)) return {$number:'-0'};
  if (Array.isArray(value)) return value.map(observe);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,observe(v)]));
  return value;
}
const cases: any[] = [];
for (const [name, nodes] of seeds) for (const context of ['root','object','array']) {
  const graph = [...nodes];
  if (context !== 'root') graph.push(context === 'object' ? {tag:'object',fields:[['value',0]]} : {tag:'array',items:[0]});
  const root = context === 'root' ? 0 : graph.length-1;
  for (const [kind, Bus] of [['permission',GatewayPermissionBus],['question',GatewayElicitationBus]] as const) {
    const bus = new Bus(); let error = null;
    const entry = {requestId:'r',toolCallId:'t',toolName:'tool',resolve(){},reject(){}};
    try { bus.register('s',entry,{payload:construct(graph,root)}); }
    catch (e: any) { error = {name:e.name,message:e.message}; }
    cases.push({name:`${kind}/${name}/${context}`,nodes:graph,root,
      expected:{error,snapshot:observe(bus.snapshot('s')),count:bus.pendingCount('s')}});
  }
}
// Synchronous validation ordering: a duplicate / invalid owner wins over invalid Date.
const ordering: any[] = [];
for (const [kind, Bus] of [['permission',GatewayPermissionBus],['question',GatewayElicitationBus]] as const) {
  for (const state of ['duplicate','shared-duplicate','empty-owner','empty-request','retry-after-error']) {
    const port = createNativeInteractionReconnectPort(); const bus = new Bus(port); const entry={requestId:'r',toolCallId:'t',toolName:'tool',resolve(){},reject(){}};
    if (state === 'duplicate') bus.register('s',entry);
    if (state === 'shared-duplicate') new Bus(port).register('s',entry);
    let error; try { bus.register(state === 'empty-owner' ? '' : 's',{...entry,requestId:state === 'empty-request' ? '' : 'r'},{payload:new Date(NaN)}); }
    catch (e: any) { error={name:e.name,message:e.message}; }
    if (state === 'retry-after-error') bus.register('s',entry,{payload:new Date(0)});
    ordering.push({kind,state,error,snapshot:observe(bus.snapshot('s')),count:bus.pendingCount('s')});
  }
}
const lifecycle: any[] = [];
for (const [kind, Bus] of [['permission',GatewayPermissionBus],['question',GatewayElicitationBus]] as const) {
  for (const cleanup of ['none','reject','bus-dispose','owner-dispose','port-dispose','register-dispose','register-consume','stale-dispose','stale-port-dispose','stale-consume','stale-reject']) {
    const port=createNativeInteractionReconnectPort(); const bus=new Bus(port);
    const binding={connectionId:'c',generation:1};
    const entry={requestId:'r',toolCallId:'t',toolName:'tool',resolve(){},reject(){}};
    let old;
    if (cleanup.startsWith('stale-')) {
      old=bus.register('s',entry);
      if (cleanup==='stale-port-dispose') port.dispose(); else port.disposeOwner('s');
    }
    const first=bus.reconnect('s',binding); let error;
    try { bus.register('s',{...entry,requestId:old ? 'new' : 'r'},{payload:new Date(NaN)}); } catch(e: any) { error={name:e.name,message:e.message}; }
    if (cleanup==='stale-dispose' || cleanup==='stale-port-dispose') old.dispose();
    if (cleanup==='stale-consume') bus.consume('s','r');
    if (cleanup==='stale-reject') bus.rejectSession('s','cleanup');
    if (cleanup==='reject') bus.rejectSession('s','cleanup');
    if (cleanup==='bus-dispose') bus.dispose();
    if (cleanup==='owner-dispose') port.disposeOwner('s');
    if (cleanup==='port-dispose') port.dispose();
    if (cleanup.startsWith('register-')) {
      const registration=bus.register('s',entry,{payload:new Date(0)});
      if (cleanup==='register-dispose') registration.dispose(); else bus.consume('s','r');
    }
    const disconnected=bus.disconnect('s',binding); const next=bus.reconnect('s',binding);
    const disconnectedAgain=bus.disconnect('s',binding); const nextAgain=bus.reconnect('s',binding);
    lifecycle.push({kind,cleanup,expected:{first,error,disconnected,next,disconnectedAgain,nextAgain}});
  }
}
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
const output = resolve(process.argv[2]); await mkdir(output,{recursive:false});
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,cases,ordering,lifecycle},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({node:process.version,sourceCommit,cases:cases.length,ordering:ordering.length,lifecycle:lifecycle.length,output}));
