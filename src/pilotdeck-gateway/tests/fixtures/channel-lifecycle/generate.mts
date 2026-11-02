// Real baseline lifecycle, with no network or account actions. Only its imported
// host/status boundaries are mocks; pilotdeckServer.ts is transpiled unchanged.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1327 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1327 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1327 PDRS_BASELINE');
assert.ok(process.argv[2], '#1327 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/cli/pilotdeckServer.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const require = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(require.resolve('tsx'))('esbuild');
const compiled = transformSync(await readFile(join(baseline, sources[0])), { loader: 'ts', format: 'cjs', target: 'es2022' }).code;
const definitions: any[] = [];
const create = (id: string, key = 'a', extra = {}) => ({ op: 'create', id, key, ...extra });
const hot = (id: string, pending?: string) => ({ op: 'hot', id, ...(pending ? { pending } : {}) });
const close = (pending?: string) => ({ op: 'close', ...(pending ? { pending } : {}) });
const wait = (id: string, event: string) => ({ op: 'wait', id, event });
const release = (gate: string) => ({ op: 'release', gate });
const receive = (id: string, message: string, gate: string) => ({ op: 'receive', id, message, gate });
const reconcile = (ids: string[], managed: string[], pending?: string) => ({ op: 'reconcile', ids, managed, ...(pending ? { pending } : {}) });
const finish = (pending: string) => ({ op: 'await', pending });
const snapshot = () => ({ op: 'snapshot' });
function scenario(name: string, steps: any[], options = {}) { definitions.push({ name, steps, ...options }); }
scenario('enable-disable-reload', [create('old'), receive('old','before','unused'), hot('old'), create('new'), hot('new'), reconcile([], ['a']), close(), close()]);
for (const mode of ['disable', 'reload', 'drain']) {
  scenario(`${mode}-inflight`, [create('old'), hot('old'), receive('old','working','turn'), create('new'),
    mode === 'disable' ? reconcile([],['a'],'op') : mode === 'reload' ? hot('new','op') : close('op'),
    wait('old','stop-begin'), receive('old','late','unused'), snapshot(), release('turn'), finish('op'), close()]);
}
scenario('late-start-after-close', [create('slow','a',{ startGate:'start' }), hot('slow','start-op'), wait('slow','start-begin'), close(), snapshot(), release('start'), finish('start-op'), hot('slow'), reconcile([],['a'])]);
scenario('failed-start-rolls-back', [create('old'), hot('old'), create('bad','a',{ failStarts:[1] }), hot('bad'), close()]);
scenario('rollback-fails', [create('old','a',{ failStarts:[2] }), hot('old'), create('bad','a',{ failStarts:[1] }), hot('bad'), close()]);
scenario('failed-stop-restored', [create('old','a',{ failStops:[1] }), hot('old'), create('new'), hot('new'), hot('new'), close()]);
scenario('queue-failure-does-not-poison', [create('bad','a',{ failStarts:[1] }), create('good'), hot('bad','bad-op'), hot('good','good-op'), finish('bad-op'), finish('good-op'), close()]);
scenario('queue-waits-and-other-key-progresses', [create('old','a',{ stopGate:'stop' }), hot('old'), create('new'), create('last'), hot('new','first'), wait('old','stop-begin'), hot('last','second'), create('other','b'), hot('other'), snapshot(), close(), snapshot(), release('stop'), finish('first'), finish('second')]);
scenario('abandoned-reconcile-continues', [create('old','a',{ stopGate:'stop' }), hot('old'), create('new','b'), reconcile(['new'],['a'],'reload'), wait('old','stop-begin'), snapshot(), { op:'drop',pending:'reload' }, release('stop'), wait('new','start-end'), snapshot(), close()]);
scenario('abandoned-hot-and-close-continue', [create('slow','a',{ startGate:'start' }), hot('slow','hot'), { op:'drop',pending:'hot' }, wait('slow','start-begin'), release('start'), wait('slow','start-end'), create('other','b',{ stopGate:'stop' }), hot('other'), close('closing'), { op:'drop',pending:'closing' }, wait('other','stop-begin'), {op:'wait-status',key:'a',state:'stopped'}, snapshot(), release('stop'), close()]);
scenario('managed-keys-are-a-set', [create('old','a',{ stopGate:'stop' }), hot('old'), create('new'), reconcile([],['a','a'],'disabled'), wait('old','stop-begin'), hot('new','restarted'), release('stop'), finish('disabled'), finish('restarted'), snapshot(), close()]);
scenario('duplicate-desired-rejected-before-stop', [create('old'), hot('old'), create('new'), reconcile(['old','new'],['a']), close()]);
scenario('close-aggregates-and-memoizes', [create('old','a',{ failStops:[1] }), hot('old'), close(), close(), hot('old')], { closeError:'host failed' });
scenario('background-startup', [create('old'), {op:'background',ids:['old']}, {op:'wait-log',text:'[adapters] channel a startup task completed'}, close()]);

async function run(definition: any) {
  const traces: Record<string, any[]> = {}, reports: Record<string, any[]> = {}, logs: any[] = [], results: any[] = [], snapshots: any[] = [];
  const channels = new Map<string, any>(), operations = new Map<string, Promise<any>>();
  const gates = new Map<string, {promise: Promise<void>, release: () => void}>();
  let hostCloses = 0;
  function gate(name: string) {
    if (!gates.has(name)) { let release!: () => void; const promise = new Promise<void>(done => release = done); gates.set(name,{promise,release}); }
    return gates.get(name)!;
  }
  const report = (key: string, update: any) => { (reports[key] ??= []).push(update); };
  const imports: Record<string, any> = {
    '../gateway/index.js': { startGatewayServer: async () => ({ close: async () => { hostCloses++; if (definition.closeError) throw new Error(definition.closeError); } }) },
    '../adapters/index.js': {}, '../pilot/index.js': {}, '../session/index.js': {},
    '../adapters/channel/protocol/ChannelRuntimeStatus.js': { createChannelRuntimeStatusReporter: () => report },
  };
  const module = { exports: {} as any };
  new Function('require','module','exports',compiled)((name: string) => {
    if (!(name in imports)) throw new Error(`#1327 unexpected import ${name}`);
    return imports[name];
  },module,module.exports);
  const original = { log:console.log, warn:console.warn, error:console.error };
  console.log = text => logs.push(['info',text]); console.warn = text => logs.push(['warn',text]); console.error = text => logs.push(['error',text]);
  const config={probe:true};
  const createServer=(channels:any[]=[])=>module.exports.startPilotDeckServer({gateway:{},config,pilotHome:'/unused-channel-probe',sessionSearch:{},channels});
  let server = definition.name === 'background-startup' ? undefined : await createServer();
  function errorValue(error: any): any {
    return { error:String(error), ...(error instanceof AggregateError ? { errors: error.errors.map(String) } : {}) };
  }
  function capture(promise: Promise<any>) { return promise.then(value => value ?? null, errorValue); }
  async function until(predicate: () => boolean) {
    for (let n=0;n<10000;n++) { if (predicate()) return; await new Promise(done => setImmediate(done)); }
    throw new Error('#1327 lifecycle hook did not resolve');
  }
  const snap = () => JSON.parse(JSON.stringify({traces,reports,hostCloses}));
  try {
    for (const step of definition.steps) {
      let promise: Promise<any> | undefined;
      if (step.op === 'create') {
        const trace = traces[step.id] = [];
        let open=false, starts=0, stops=0;
        const flights = new Set<Promise<void>>();
        channels.set(step.id, {
          channelKey:step.key,
          async start(deps: any) {
            starts++; trace.push({event:'start-begin'});
            assert.equal(deps.config,config,'#1327 config identity boundary');
            if (step.startGate) await gate(step.startGate).promise;
            if (step.failStarts?.includes(starts)) throw new Error(`${step.id} start failed`);
            open=true; trace.push({event:'start-end'});
            return { stop(reason: string) {
              open=false; stops++; trace.push({event:'stop-begin',reason});
              return (async () => {
                if (step.stopGate) await gate(step.stopGate).promise;
                if (step.failStops?.includes(stops)) throw new Error(`${step.id} stop failed`);
                await Promise.all([...flights]);
                trace.push({event:'stop-end',reason});
              })();
            } };
          },
          receive(message: string, gateName: string) {
            trace.push({event:open?'accepted':'rejected',message});
            if (!open) return;
            const task = gate(gateName).promise.then(() => { trace.push({event:'complete',message}); flights.delete(task); });
            flights.add(task);
          },
        });
      } else if (step.op === 'hot') promise=server.hotStartChannel(channels.get(step.id));
      else if (step.op === 'reconcile') promise=server.reconcileChannels({channels:step.ids.map((id:string)=>channels.get(id)),managedChannelKeys:step.managed});
      else if (step.op === 'close') promise=server.close();
      else if (step.op === 'background') {
        // Same startup entry in the real module, on a fresh server for this case.
        assert.equal(channels.size,1);
        server=await createServer(step.ids.map((id:string)=>channels.get(id)));
      }
      else if (step.op === 'receive') channels.get(step.id).receive(step.message,step.gate);
      else if (step.op === 'release') gate(step.gate).release();
      else if (step.op === 'wait') await until(()=>traces[step.id].some(item=>item.event===step.event));
      else if (step.op === 'wait-status') await until(()=>reports[step.key]?.at(-1)?.state===step.state);
      else if (step.op === 'wait-log') await until(()=>logs.some(item=>item[1]===step.text));
      else if (step.op === 'snapshot') snapshots.push(snap());
      else if (step.op === 'drop') operations.delete(step.pending);
      else if (step.op === 'await') results.push(await operations.get(step.pending));
      else throw new Error(`#1327 unknown step ${step.op}`);
      if (promise) {
        const captured=capture(promise);
        if (step.pending) operations.set(step.pending,captured); else results.push(await captured);
      }
    }
    // A barrier after the final lifecycle completion flushes detached observers.
    await new Promise(done=>setImmediate(done));
    return {traces,reports,results,snapshots,logs,hostCloses};
  } finally { Object.assign(console,original); }
}
const cases=[];
for (const definition of definitions) {
  let timer:ReturnType<typeof setTimeout>|undefined;
  try {
    const timeout=new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error(`#1327 scenario timeout: ${definition.name}`)),5000);});
    cases.push({...definition,expected:await Promise.race([run(definition),timeout])});
  } finally {clearTimeout(timer);}
}
await mkdir(process.argv[2],{recursive:true});
await writeFile(join(process.argv[2],'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,icu:process.versions.icu,cases},null,2)+'\n',{flag:'wx'});
console.log(`PASS #1327 lifecycle: ${cases.length} real TS scenarios`);
