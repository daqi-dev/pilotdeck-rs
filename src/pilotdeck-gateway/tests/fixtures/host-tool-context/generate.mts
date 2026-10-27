import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Real TS constructors only. No Rust output is used to manufacture expectations.
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const sources = [
  'src/agent/modules/checkpoint/hostToolCheckpoint.ts',
  'src/agent/modules/checkpoint/seedStateProjection.ts',
  'src/agent/modules/transport/sidecarToolContext.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'src/agent/turn/TurnEnvironment.ts',
];
function clean() { assertBaseline(baseline); }
async function hashes() { return baselineSourceHashes(baseline, sources); }
clean();
const sourceSha256 = await hashes();
const load = (source: string) => import(pathToFileURL(join(baseline, source)).href);
const { HostToolCheckpoint } = await load(sources[0]);
const { parseAgentLoopSeedStateProjection: parse, serializeAgentLoopSeedStateProjection: serialize } = await load(sources[1]);
const { createSidecarToolContextBuilder: builder, createSidecarToolRuntimeServices: runtimeService,
  createSidecarPermissionRequestContextServices: permissionService, createSidecarContextRequestIdentityServices: identityService } = await load(sources[2]);
const { createSidecarDefaultModuleDispatcher: dispatcher } = await load(sources[3]);
const { buildTurnEnvironment } = await load(sources[4]);
const owned = await mkdtemp(join(tmpdir(), 'pdrs-host-tool-context-'));
const originalCwd = process.cwd();
const originalEnv = process.env;
const json = (value: any) => JSON.parse(JSON.stringify(value, (_, item) => item instanceof Map ? Object.fromEntries(item) : item));
const capture = (f: () => any) => {
  try { return json({ ok: true, value: f() }); }
  catch (error) { return { ok: false, error: (error as Error).message }; }
};
function decode(value: any): any {
  if (Array.isArray(value)) return value.map(decode);
  if (value && typeof value === 'object') {
    if (value.$number) return { nan: NaN, infinity: Infinity, negativeInfinity: -Infinity }[value.$number];
    if (value.$undefined) return undefined;
    if (value.$nonplain) return new Date(0);
    const object = Object.fromEntries(Object.entries(value).map(([key, v]) => [key, decode(v)]));
    if (value.$prototype) { delete object.$prototype; Object.setPrototypeOf(object, decode(value.$prototype)); }
    if (value.$nullPrototype) { delete object.$nullPrototype; return Object.assign(Object.create(null), object); }
    return object;
  }
  return value;
}
const cases: any[] = [];
function seed(name: string, input: any, maps = false) {
  const actual = decode(input);
  if (maps && actual) for (const key of ['readFileState', 'writeSnapshots']) {
    if (actual[key]) actual[key] = new Map(Object.entries(actual[key]).reverse());
  }
  cases.push({ kind: 'seed', name, input, maps, expected: capture(() => {
    const parsed = parse(actual);
    return parsed && {projection:serialize(parsed),readKeys:[...(parsed.readFileState?.keys() ?? [])],writeKeys:[...(parsed.writeSnapshots?.keys() ?? [])],
      ...(name === 'number-nonfinite' ? {nonfinite:[Number.isNaN(parsed.readFileState.get('\ud800').mtimeMs),parsed.readFileState.get('\ud800').offset === Infinity,parsed.writeSnapshots.get('f').mtimeMs === -Infinity]} : {})};
  }) });
}
try {
  process.chdir(owned);
  // Missing config.env is tested without ever capturing the user's environment.
  process.env = { FIXTURE_DEFAULT: 'yes' };
  for (const [name, value] of Object.entries({ missing: {$undefined:true}, null: null, empty:{}, array:[], number:3, string:'x', false:false, nonplain:{$nonplain:true}, nullproto:{$nullPrototype:true}, ignored:{extra:1} })) seed(name, value);
  for (const value of [null, 3, 'x', {}, ['ok', 3], ['a','a','', '\ud800'], []]) seed(`allowed-${JSON.stringify(value)}`, {allowedReadFiles:value});
  for (const kind of ['text','image','pdf','notebook','bad',null]) seed(`read-kind-${kind}`, {readFileState:{file:{mtimeMs:1,kind,offset:0,limit:-2,pages:'1-3',extra:1}}});
  for (const key of ['readFileState','writeSnapshots']) {
    for (const value of [null, [], 3, {file:null}]) seed(`${key}-${JSON.stringify(value)}`, {[key]:value});
  }
  seed('read-option-filter', {readFileState:{file:{mtimeMs:1,kind:'text',offset:'bad',limit:null,pages:3,extra:true}}});
  seed('write-full', {writeSnapshots:{file:{absolutePath:'',mtimeMs:0,contentHash:'',offset:-1,limit:0,extra:3}}});
  seed('write-invalid', {writeSnapshots:{file:{absolutePath:'file',mtimeMs:'1',contentHash:'hash'}}});
  seed('number-nonfinite', {readFileState:{'\ud800':{mtimeMs:{$number:'nan'},kind:'text',offset:{$number:'infinity'}}},writeSnapshots:{f:{absolutePath:'f',mtimeMs:{$number:'negativeInfinity'},contentHash:'h'}}});
  seed('native-maps', {readFileState:{'10':{mtimeMs:1,kind:'text'},'2':{mtimeMs:2,kind:'pdf'}},writeSnapshots:{f:{absolutePath:'f',mtimeMs:0,contentHash:'h'}}}, true);
  seed('nonplain-seed', {$prototype:{allowedReadFiles:['inherited']}});
  seed('nonplain-entry', {readFileState:{file:{$nonplain:true}}});

  const checkpointSpecs = [
    {name:'empty', seed:{}, allowed:[], ops:[]},
    {name:'ordered-grants',seed:{allowedReadFiles:['b','a','b']},allowed:['a','c'],ops:[{op:'allow',paths:['d','b','']},{op:'allow',paths:[]}]},
    {name:'shared-maps-detached-grants',seed:{readFileState:{f:{mtimeMs:1,kind:'text'}}},allowed:['a'],ops:[{op:'read',path:'g',value:{mtimeMs:2,kind:'image',extra:'drop'}},{op:'write',path:'w',value:{absolutePath:'/w',mtimeMs:3,contentHash:'h',extra:true}},{op:'allow',paths:['b']}]},
    {name:'numeric-map-order',seed:{},allowed:[],ops:['b','10','2','01','4294967295'].map(path=>({op:'read',path,value:{mtimeMs:1,kind:'text'}}))},
    {name:'native-seed-map',nativeSeedMaps:true,seed:{readFileState:{'2':{mtimeMs:2,kind:'text'},'10':{mtimeMs:10,kind:'pdf'}}},allowed:[],ops:[]},
  ];
  for (const spec of checkpointSpecs) {
    const parsed = parse(spec.seed);
    if (spec.nativeSeedMaps) parsed.readFileState = new Map([...parsed.readFileState].reverse());
    const checkpoint = new HostToolCheckpoint(parsed, spec.allowed);
    const before = checkpoint.toolContextState();
    const detached = checkpoint.snapshot();
    parsed.readFileState?.clear();
    for (const op of spec.ops) {
      if (op.op === 'allow') checkpoint.allowReadFiles(op.paths);
      if (op.op === 'read') before.readFileState.set(op.path, op.value);
      if (op.op === 'write') before.writeSnapshots.set(op.path, op.value);
    }
    const after = checkpoint.toolContextState();
    const snapshot = checkpoint.snapshot();
    const expected = json({ before, after, detached:serialize(detached), snapshot:serialize(snapshot), readKeys:[...before.readFileState.keys()], snapshotReadKeys:[...snapshot.readFileState.keys()], sameRead:before.readFileState === after.readFileState, sameWrite:before.writeSnapshots === after.writeSnapshots });
    cases.push({kind:'checkpoint',...spec,expected});
  }

  const baseConfig = {cwd:'/fixture/project',permissionMode:'default',permissionContext:{canPrompt:true,rules:[{allow:'read'}],cwd:'spoof',mode:'spoof',custom:7},env:{BASE:'value',PILOTDECK_SESSION_ID:'spoof'}};
  const baseInput = {sessionId:'session',turnId:'turn'};
  const specs: any[] = [];
  const add = (name: string, patch: any = {}) => specs.push({name,...patch});
  add('minimal'); add('all-services',{ports:true,planTodo:true,oneShot:true,abort:true,config:{includeToolProgress:true},input:{canElicit:true,canPrompt:false,execution:{operationDeadline:100}}});
  add('permission-services',{ports:true,view:'permission',abort:true,config:{includeToolProgress:true},input:{canElicit:true}});
  add('identity-service',{ports:true,view:'identity',abort:true,remote:{sessionId:'spoof',cwd:'spoof',extra:1,abortSignal:'remote'}});
  add('runtime-service',{ports:true,view:'runtime',oneShot:true});
  add('aux-disabled',{ports:true,aux:false,oneShot:true}); add('model-fallback',{ports:true,modelFallback:true});
  add('plan-empty',{ports:true,emptyPlan:true}); add('progress-disabled',{ports:true});
  add('identity-remote-abort',{remote:{abortSignal:'remote',custom:1,sessionId:'spoof'}});
  add('remote-authority',{ports:true,remote:{sessionId:'evil',turnId:'evil',cwd:'evil',permissionMode:'bypass',permissionContext:{},env:{EVIL:'1'},allowedReadFiles:['evil'],model:'evil',subagent:'evil',currentToolCallId:'call',outputTruncated:false}});
  for (const value of [null, false, true, 0, '', [], {}, {$undefined:true}]) add(`output-${JSON.stringify(value)}`,{remote:{outputTruncated:value}});
  for (const value of [null, 7, '', '\ud800']) add(`call-id-${JSON.stringify(value)}`,{remote:{currentToolCallId:value}});
  for (const value of [null, [], 'x', 4]) add(`remote-${JSON.stringify(value)}`,{remote:value});
  for (const value of [0,-1,null]) add(`config-optional-${value}`,{config:{subagentTimeoutMs:value,maxResultBytes:value,maxOutputTokens:value,subagentDepth:value,modelMultimodal:value,toolAliases:value,runMode:value},input:{canPrompt:value,execution:{operationDeadline:value}},call:{idempotencyKey:value}});
  add('empty-objects-truthy',{config:{toolAliases:{},modelMultimodal:{},runMode:''},call:{idempotencyKey:'key'}});
  add('elicitation-not-true',{ports:true,input:{canElicit:1}});
  add('elicitation-unavailable',{input:{canElicit:true}});
  add('default-environment',{config:{env:{$undefined:true}},input:{sessionId:'...中文...',turnId:'Ａ／B'},call:{idempotencyKey:''}});
  add('environment-unicode',{input:{sessionId:' A\u0301..foo ',turnId:'Ｚ'.repeat(100)}});
  add('remote-prototype',{remote:{$prototype:{currentToolCallId:'inherited',outputTruncated:true,permissionMode:'spoof'},custom:'own'}});
  for (const spec of specs) {
    const config = {...baseConfig,...decode(spec.config ?? {})};
    const input: any = {...baseInput,...decode(spec.input ?? {}),...(spec.abort ? {abortSignal:{handle:'abort'}} : {})};
    const checkpoint = new HostToolCheckpoint(parse({allowedReadFiles:['seed'],readFileState:{f:{mtimeMs:1,kind:'text'}}}));
    const trace: any[] = [];
    const record = (event: string, fields: any = {}) => trace.push(json({event,...fields}));
    const handle = (name: string) => ({handle:name});
    const ports = spec.ports ? {
      model:{...handle('model'),forTurn:(identity: any) => {record('model', {identity});return spec.modelFallback ? undefined : handle('bound-model');}},
      interaction:{elicitation:handle('elicitation'),userDialog:handle('dialog')},
      toolExecution:{auditRecorder:handle('audit'),fileHistory:handle('history'),fileUpdateNotifier:handle('notifier')},
      clock:{now:()=> 'fixture-now'},eventEmitter:(event: any)=>record('progress',{value:event}),
      goal:{forSession:(sessionId: string)=>{record('goal',{sessionId});return handle('goal');}},
      subagent:{oneShot:{createForkApi:(binding: any)=>{record('fork',{identity:{sessionId:binding.sessionId,turnId:binding.turnId},sameRead:binding.parentReadFileState===checkpoint.readFileState,sameWrite:binding.parentWriteSnapshots===checkpoint.writeSnapshots});return handle('subagent');}}},
      planMode:{planFileManager:{getPlanDirectoryPath:()=>{record('directory');return spec.emptyPlan ? '' : '/plans';},resolvePlanFilePath:(path: string,cwd: string)=>{record('resolve',{path,cwd});return `${cwd}/${path}`;},readPlanFile:(path: string,cwd: string)=>{record('read',{path,cwd});return 'plan-content';}}},
    } : {};
    const binding = {config,input,checkpoint};
    const context = spec.view === 'permission' ? permissionService(ports).bindTurn(binding)
      : spec.view === 'identity' ? identityService(ports).bindTurn(binding)
      : spec.view === 'runtime' ? runtimeService(ports).bindTurn(binding)
      : builder({...binding,ports,includeAuxiliaryModel:spec.aux});
    const remote = decode(spec.remote ?? {});
    const call = {runId:'run',operationId:'operation',sessionId:'evil',turnId:'evil',...decode(spec.call ?? {})};
    const environment = buildTurnEnvironment(config.env, config.cwd, input.sessionId, input.turnId);
    const expected = capture(() => {
      const result: any = {};
      if (context.permissionContext) result.permission = context.permissionContext();
      if (context.contextIdentity) result.identity = context.contextIdentity(remote && typeof remote === 'object' && !Array.isArray(remote) ? remote : undefined);
      if (context.executionContext) result.execution = context.executionContext(call);
      if (context.toolRuntimeContext) {
        const runtime = context.toolRuntimeContext(remote, ...(spec.view === 'permission' ? [] : [spec.planTodo ? handle('plan') : undefined, spec.oneShot === true]));
        result.runtime = {...runtime,...(runtime.now ? {now:runtime.now()} : {}),...(runtime.progress ? {progress:true} : {}),...(runtime.planDirectory ? {planDirectory:{path:runtime.planDirectory.path,resolve:runtime.planDirectory.resolve('x'),read:runtime.planDirectory.read('x')}} : {})};
        runtime.progress?.({toolCallId:'c',toolName:'read',message:'working',metadata:{percent:50},createdAt:'now',sessionId:'evil',extra:true});
      }
      return result;
    });
    cases.push({kind:'context',...spec,environment,expected,trace});
  }

  for (const spec of [
    {name:'unknown',messages:[],apply:['missing']},
    {name:'delayed-repeated',messages:[{itemId:'a',allowedReadFiles:['x','x',3,'y']}],apply:['missing','a','a']},
    {name:'overwrite-empty-preserves',messages:[{itemId:'a',allowedReadFiles:['old']},{itemId:'a',allowedReadFiles:['new']},{itemId:'a',allowedReadFiles:[]}],apply:['a']},
  ]) {
    const checkpoint = new HostToolCheckpoint(undefined,['seed']);
    const d = dispatcher({config:baseConfig,input:{...baseInput,messages:[],drainSteerMessages:async()=>spec.messages},checkpoint,modules:{model:{execution:{}},capability:{execution:{list:()=>[]},runtimeContext:runtimeService({})}},capabilityResultObserver:{onCapabilityResults:async()=>{}},planTodoHandler:async()=>({})});
    await d.handlers.turn({payload:{operation:'drain_steer'}});
    const snapshots = [serialize(checkpoint.snapshot())];
    for (const id of spec.apply) { d.applySteerAuthorization(id); snapshots.push(serialize(checkpoint.snapshot())); }
    await d.dispose();
    cases.push({kind:'steer',...spec,expected:json(snapshots)});
  }
  assert.equal(new Set(cases.map(c=>`${c.kind}/${c.name}`)).size,cases.length);
  assert.equal(cases.length, 82);
  assert.deepEqual(['seed','checkpoint','context','steer'].map(kind=>cases.filter(c=>c.kind===kind).length),[38,5,36,3]);
  process.chdir(originalCwd);
  process.env = originalEnv;
  clean();
  assert.deepEqual(await hashes(),sourceSha256,'source changed while recording');
  await writeFile(join(owned,'oracle.json'),JSON.stringify({commit,sourceSha256,cases},null,2)+'\n');
  await mkdir(output,{recursive:true});
  await writeFile(join(output,'oracle.json'),await readFile(join(owned,'oracle.json')));
  console.log(`HOST_TOOL_CONTEXT_ORACLE_PASS cases=${cases.length}`);
} finally {
  process.chdir(originalCwd);
  process.env = originalEnv;
  await rm(owned,{recursive:true,force:true});
}
