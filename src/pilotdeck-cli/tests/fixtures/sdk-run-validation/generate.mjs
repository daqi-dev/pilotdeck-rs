// Direct, deterministic calls into the pinned SDK; no sockets or query lifecycle.
import assert from 'node:assert/strict';
import {readFileSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
assert.equal(process.version, 'v22.22.0', '#1058 validation Node version');
assert.equal(process.argv.length, 3, '#1058 validation OUTPUT');
const baseline = resolve(process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck');
const {createQueryWithTransport} = await import(pathToFileURL(join(baseline, 'packages/sdk/dist/client.js')));
const {GatewayTransport} = await import(pathToFileURL(join(baseline, 'packages/sdk/dist/transport.js')));
const valid = {id:'alias', provider:'fixture', model:'model'};
const definitions = [
  {name:'disabled', model:'alias', items:[{...valid, available:false}]},
  {name:'disabled-sibling', model:'alias', items:[{...valid, provider:'wrong', available:false}, valid]},
  {name:'ambiguous', model:'model', items:[valid, {...valid, provider:'other'}]},
  {name:'missing-provider', model:'alias', items:[{id:'alias', model:'model'}]},
  {name:'missing-model', model:'alias', items:[{id:'alias', provider:'fixture'}]},
  {name:'missing-both', model:'undefined/undefined', items:[{}]},
  // The missing-field item matches the literal pair, making this ambiguous.
  {name:'undefined-pair-sibling', model:'undefined/undefined', items:[{}, {...valid, id:'undefined/undefined'}]},
  {name:'empty-provider', model:'alias', items:[{...valid, provider:''}]},
  {name:'empty-model', model:'alias', items:[{...valid, model:''}]},
  {name:'by-pair', model:'fixture/model', items:[valid]},
  {name:'by-id', model:'alias', items:[valid]},
  {name:'by-name', model:'model', items:[valid]},
];
const models = [];
for (const input of definitions) {
  const requests = [];
  const query = createQueryWithTransport('', {projectKey:'/d1-project'}, {
    async request(method, params) { requests.push({method, params}); return {items:input.items}; },
  });
  let output;
  try { output = {value:await query.resolveModelOverride(input.model)}; }
  catch (error) { output = {error:error.message}; }
  assert.deepEqual(requests, [{method:'model_catalog_list', params:{projectKey:'/d1-project'}}], '#1058 real model method');
  models.push({input, output});
  console.log(JSON.stringify({input, output}));
}
const sequences = [];
for (const input of [
  {name:'first', previous:-1, seq:0}, {name:'next', previous:0, seq:1},
  {name:'duplicate', previous:0, seq:0}, {name:'older', previous:1, seq:0},
  {name:'gap-first', previous:-1, seq:1}, {name:'gap-later', previous:0, seq:2},
  {name:'negative', previous:-1, seq:-1}, {name:'fractional', previous:-1, seq:0.5},
  {name:'missing', previous:-1}, {name:'null', previous:-1, seq:null},
  {name:'string', previous:-1, seq:'0'}, {name:'boolean', previous:-1, seq:false},
]) {
  const transport = new GatewayTransport({url:'unused', token:'unused'});
  let output = {sequence:null};
  transport.streams.set('validation', {
    push(event) { output = {sequence:event.sequence}; },
    fail(error) { output = {error:error.message}; },
    close() { throw new Error('#1058 validation never sends final'); },
  });
  transport.lastSeq.set('validation', input.previous);
  transport.handleMessage(JSON.stringify({type:'event', id:'validation', seq:input.seq, event:{type:'assistant_text_delta', text:'sample'}}));
  sequences.push({input, output});
  console.log(JSON.stringify({input, output}));
}
const files = ['packages/sdk/src/client.ts', 'packages/sdk/dist/client.js', 'packages/sdk/src/transport.ts', 'packages/sdk/dist/transport.js'];
const oracle = {nodeVersion:process.version, kind:'real-sdk-method-validation-no-lifecycle',
  sourceSha256:Object.fromEntries(files.map(file => [file, createHash('sha256').update(readFileSync(join(baseline,file))).digest('hex')])),
  models, sequences};
const output = resolve(process.argv[2]);
mkdirSync(output, {recursive:true});
writeFileSync(join(output,'oracle.json'), JSON.stringify(oracle,null,2)+'\n', {flag:'wx'});
