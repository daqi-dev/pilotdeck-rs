// PDRS_BASELINE=<fixed checkout> node --import <baseline>/node_modules/tsx/dist/loader.mjs generate.mts <output-parent>
// Each run creates an exclusive mkdtemp directory under output-parent. No deletes.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output parent directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
const sources = [
  'src/tool/registry/ToolRegistry.ts',
  'src/tool/execution/repairToolName.ts',
  'src/tool/execution/errorRecovery.ts',
  'src/tool/execution/ToolRuntime.ts',
  'src/tool/protocol/errors.ts',
  'src/agent/modules/capability/executionDeadline.ts',
  'src/tool/protocol/subagentTimeout.ts',
];
const sourceSha256 = Object.fromEntries(await Promise.all(sources.map(async source => [source,
  createHash('sha256').update(await readFile(join(baseline, source))).digest('hex'),
])));
const load = (source: string) => import(pathToFileURL(join(baseline, source)).href);
const { ToolRegistry } = await load(sources[0]);
const { repairToolName } = await load(sources[1]);
const { buildToolErrorRecovery } = await load(sources[2]);
const { createToolErrorResult } = await load(sources[3]);
const { withOperationDeadlineBudget } = await load(sources[5]);
const instant = '2026-09-11T00:00:00.000Z';
const recoveryInputs = JSON.parse(await readFile(join(dirname(fileURLToPath(import.meta.url)), 'recovery-inputs.json'), 'utf8'));
assert.equal(recoveryInputs.length, 75);
const recovery = recoveryInputs.map((input: any) => ({ input, output: buildToolErrorRecovery(input),
  result: createToolErrorResult({ toolCallId: input.id, toolName: input.toolName, code: input.code,
    message: input.message, details: input.details, startedAt: instant,
    context: { cwd: input.cwd, permissionMode: input.permissionMode, now: () => new Date(instant) },
  }),
}));

// Alias spellings are inputs only: the result always comes from real TS repairToolName.
const aliasGroups: Array<[string[], string[]]> = [
  [['run','shell','sh','terminal','command','exec','execute'], ['bash']],
  [['write','write_file','writefile','create_file','createfile','save_file','savefile'], ['write_file','write']],
  [['read','read_file','readfile','open_file','openfile','cat_file','catfile'], ['read_file','read']],
  [['edit','edit_file','editfile','modify_file','modifyfile','patch_file','patchfile','replace_file','replacefile'], ['edit_file','edit']],
  [['web_fetch','webfetch'], ['web_fetch','webfetch']],
  [['fetch_url','fetchurl','url_fetch','urlfetch'], ['web_fetch']],
  [['web_search','websearch'], ['web_search','websearch']],
  [['search_web','searchweb'], ['web_search']],
  [['todo_write','todowrite'], ['todo_write','todowrite']],
  [['todo','update_todo','updatetodo'], ['todo_write']],
  [['ask_user','ask_user_question'], ['ask_user_question','ask_user']],
  [['askuser','ask_question','askquestion'], ['ask_user_question']],
  [['task','subagent'], ['agent']],
  [['edit_notebook','notebook_edit','notebookedit'], ['edit_notebook','notebook_edit']],
  [['editnotebook'], ['edit_notebook']],
  [['enter_plan_mode','enterplanmode'], ['enter_plan_mode','enterplanmode']],
  [['exit_plan_mode','exitplanmode'], ['exit_plan_mode','exitplanmode']],
  [['list_mcp_resources','listmcpresources'], ['list_mcp_resources','listmcpresources']],
  [['read_mcp_resource','readmcpresource'], ['read_mcp_resource','readmcpresource']],
];
const nameInputs: any[] = [];
for (const [spellings, targets] of aliasGroups) {
  for (const raw of spellings) {
    nameInputs.push({ id: `builtin-${raw}`, raw, tools: targets.map(name => ({ name })) });
    if (targets.length > 1) nameInputs.push({ id: `fallback-${raw}`, raw, tools: [{ name: targets[1] }] });
  }
}
nameInputs.push(
  { id:'blank',raw:' \t\ufeff ',tools:[{name:'read_file'}] },
  { id:'normalized-camel',raw:'ReadFile',tools:[{name:'read_file'}] },
  { id:'normalized-acronym',raw:'HTTPFetch',tools:[{name:'http_fetch'}] },
  { id:'normalized-path',raw:'functions.Tools/ReadFile',tools:[{name:'read_file'}] },
  { id:'normalized-backslash',raw:'tools\\ReadFile',tools:[{name:'read_file'}] },
  { id:'normalized-colons',raw:'functions::read_file',tools:[{name:'read_file'}] },
  { id:'normalized-repeat-affixes',raw:'tool_functions_builtin_read_file_tools_function',tools:[{name:'read_file'}] },
  { id:'configured-first',raw:'read',tools:[{name:'read_file'},{name:'write_file'}],configured:{read:'write_file'} },
  { id:'configured-normalized',raw:'Tools.Read',tools:[{name:'read_file'},{name:'write_file'}],configured:{read:'write_file'} },
  { id:'configured-target-missing',raw:'read',tools:[{name:'read_file'},{name:'write_file'}],configured:{read:'missing',Read:'write_file'} },
  { id:'fuzzy-one',raw:'read_flie',tools:[{name:'read_file'}] },
  { id:'fuzzy-tie',raw:'map',tools:[{name:'cap'},{name:'tap'}] },
  { id:'fuzzy-short-rejected',raw:'ab',tools:[{name:'ac'}] },
  { id:'fuzzy-short-accepted',raw:'cat',tools:[{name:'car'}] },
  { id:'fuzzy-too-far',raw:'zzzzzzz',tools:[{name:'read_file'}] },
  { id:'alias-spelling',raw:'get_notes',tools:[{name:'read_file',aliases:['getNotes']}] },
  { id:'alias-no-false-tie',raw:'read_flie',tools:[{name:'read_file',aliases:['ReadFile','read-file']}] },
  { id:'normalized-ambiguity',raw:'ReadFile',tools:[{name:'read_file'},{name:'read-file'}] },
  { id:'additional-unavailable',raw:'SearchWeb',tools:[],additional:[{name:'web_search',aliases:['search']}] },
  { id:'surrogate-normalization',raw:'tool_\ud800read_file',tools:[{name:'read_file'}] },
  // Round 5: non-ASCII spellings near the fuzzy threshold.
  { id:'fuzzy-non-ascii-dropped',raw:'réadfiles',tools:[{name:'read_file'}] },
  { id:'fuzzy-non-ascii-inner',raw:'réad_file',tools:[{name:'read_file'}] },
  { id:'fuzzy-non-ascii-tail',raw:'readfilé',tools:[{name:'read_file'}] },
  { id:'fuzzy-non-ascii-transposed',raw:'ŕead_flie',tools:[{name:'read_file'}] },
);
const names = nameInputs.map(input => ({ input, output: repairToolName(input.raw, input.tools, input.configured, input.additional ?? []) ?? null }));

const deadlineInputs = [
  {id:'none'}, {id:'empty',deadline:''}, {id:'malformed',deadline:'not-a-date',configured:10000},
  {id:'expired',deadline:'2026-09-10T23:59:59.999Z',configured:10000},
  {id:'equal',deadline:instant,configured:10000},
  {id:'shorten',deadline:'2026-09-11T00:00:02.000Z',configured:10000},
  {id:'preserve-short',deadline:'2026-09-11T00:00:02.000Z',configured:500},
  {id:'default-far',deadline:'2026-09-11T02:00:00.000Z'},
  {id:'default-near',deadline:'2026-09-11T00:00:02.000Z'},
  {id:'invalid-now',deadline:'2026-09-11T00:00:02.000Z',now:'invalid',configured:10000},
  {id:'nan-configured',deadline:'2026-09-11T00:00:02.000Z',configured:'NaN'},
  {id:'infinite-configured',deadline:'2026-09-11T00:00:02.000Z',configured:'Infinity'},
  {id:'negative-configured',deadline:'2026-09-11T00:00:02.000Z',configured:-20},
  {id:'zero-configured',deadline:'2026-09-11T00:00:02.000Z',configured:0},
  {id:'offset',deadline:'2026-09-11T08:00:02.000+08:00',configured:10000},
  {id:'fractional',deadline:'2026-09-11T00:00:00.001Z',configured:0.5},
  // `configuredRaw` is stored as-is (no NaN/Infinity decoding).
  {id:'string-configured',deadline:'2026-09-11T00:00:02.000Z',configuredRaw:'5000'},
  {id:'null-configured',deadline:'2026-09-11T00:00:02.000Z',configuredRaw:null},
  {id:'boolean-configured',deadline:'2026-09-11T00:00:02.000Z',configuredRaw:true},
  {id:'object-configured',deadline:'2026-09-11T00:00:02.000Z',configuredRaw:{ms:1}},
];
const encodeNumber = (value: any) => typeof value === 'number' && !Number.isFinite(value) ? String(value) : value;
const deadlines = deadlineInputs.map((input: any) => {
  const context: any = { now: () => new Date(input.now ?? instant) };
  if ('configured' in input) context.subagentTimeoutMs = typeof input.configured === 'string' ? Number(input.configured) : input.configured;
  if ('configuredRaw' in input) context.subagentTimeoutMs = input.configuredRaw;
  const result = withOperationDeadlineBudget(context, { operationDeadline: input.deadline });
  return {input,output:{hasTimeout:'subagentTimeoutMs' in result,...('subagentTimeoutMs' in result ? {timeout:encodeNumber(result.subagentTimeoutMs)} : {})}};
});

const steps: any[] = [
  {op:'create',r:'root'},
  {op:'register',r:'root',handle:'a',tool:{name:'alpha',aliases:['A'],description:'original'}},
  {op:'register',r:'root',handle:'b',tool:{name:'beta',aliases:['B']}},
  {op:'register',r:'root',tool:{name:'alpha'}},
  {op:'register',r:'root',tool:{name:'A'}},
  {op:'register',r:'root',tool:{name:'gamma',aliases:['beta']}},
  {op:'register',r:'root',tool:{name:'gamma',aliases:['A']}},
  {op:'get',r:'root',name:'A'}, {op:'has',r:'root',name:'missing'},
  {op:'unregister',r:'root',name:'A'},
  {op:'scope',r:'child',parent:'root',denied:['beta']},
  {op:'inspect',r:'child'},
  {op:'upsert',r:'child',handle:'shadow',tool:{name:'alpha',description:'shadow'}},
  {op:'get',r:'child',name:'A'}, {op:'get',r:'root',name:'alpha'},
  {op:'hide',r:'root',name:'A'}, {op:'isHidden',r:'root',name:'A'},
  {op:'get',r:'root',name:'alpha'}, {op:'inspect',r:'root'},
  {op:'snapshot',r:'copy',parent:'root'}, {op:'inspect',r:'copy'},
  {op:'isHidden',r:'copy',name:'alpha'}, {op:'isHidden',r:'child',name:'alpha'},
  {op:'reveal',r:'root',name:'A'}, {op:'reveal',r:'root',name:'A'},
  {op:'markUnavailable',r:'root',diagnostic:{toolName:'optional',code:'setup_required',reason:'parent reason'},aliases:['z','a']},
  {op:'markUnavailable',r:'child',diagnostic:{toolName:'optional',code:'failed_check',reason:'child reason'},aliases:['c','a']},
  {op:'unavailable',r:'child',name:'optional'}, {op:'inspect',r:'child'},
  {op:'register',r:'root',handle:'optional',tool:{name:'optional',aliases:['z']}},
  {op:'unavailable',r:'root',name:'a'}, {op:'unavailable',r:'root',name:'z'},
  {op:'replace',r:'root',handle:'replacement',tool:{name:'alpha',aliases:['newA'],description:'new'}},
  {op:'handle',handle:'a'}, {op:'handleDispose',handle:'a'},
  {op:'get',r:'root',name:'alpha'}, {op:'get',r:'root',name:'A'},
  {op:'get',r:'child',name:'newA'},
  {op:'replace',r:'root',tool:{name:'not_registered'}},
  {op:'upsert',r:'root',handle:'betaReplacement',tool:{name:'beta',aliases:['newA']}},
  {op:'get',r:'root',name:'newA'}, {op:'handle',handle:'b'},
  {op:'unregister',r:'root',name:'alpha'}, {op:'get',r:'root',name:'newA'},
  {op:'register',r:'root',handle:'self',tool:{name:'self',aliases:['self','duplicate','duplicate']}},
  {op:'get',r:'root',name:'duplicate'}, {op:'schemas',r:'root'},
  {op:'hide',r:'root',name:'self'}, {op:'hide',r:'root',name:'missing'},
  {op:'dispose',r:'child'}, {op:'inspect',r:'child'}, {op:'handle',handle:'shadow'},
  {op:'inspect',r:'root'}, {op:'handleDispose',handle:'self'}, {op:'handle',handle:'self'},
  {op:'dispose',r:'root'}, {op:'dispose',r:'root'}, {op:'inspect',r:'root'},
  {op:'isHidden',r:'root',name:'self'}, {op:'register',r:'root',tool:{name:'late'}},
  {op:'replace',r:'root',tool:{name:'beta'}}, {op:'unregister',r:'root',name:'beta'},
  {op:'markUnavailable',r:'root',diagnostic:{toolName:'x',code:'unavailable',reason:'x'}},
  {op:'snapshot',r:'after',parent:'root'}, {op:'scope',r:'after',parent:'root'},
  {op:'get',r:'copy',name:'beta'},
];
function tool(spec: any) { return {...spec,description:spec.description ?? spec.name,aliases:spec.aliases ?? [],kind:'custom',inputSchema:{type:'object'},isReadOnly:()=>true,isConcurrencySafe:()=>true,execute:async()=>({content:[]})}; }
function descriptor(t: any) { return t ? {name:t.name,description:t.description,aliases:t.aliases} : null; }
const registries = new Map<string, any>();
const handles = new Map<string, any>();
const registry = steps.map(input => {
  try {
    const r = registries.get(input.r); let value: any = null;
    switch(input.op) {
      case 'create':registries.set(input.r,new ToolRegistry());break;
      case 'scope':registries.set(input.r,registries.get(input.parent).createScopedView({evaluate:(t:any)=>({allowed:!(input.denied??[]).includes(t.name)})}));break;
      case 'snapshot':registries.set(input.r,registries.get(input.parent).clone());break;
      case 'register':case 'replace':case 'upsert': {
        const handle=r[input.op==='upsert'?'registerOrReplace':input.op](tool(input.tool));
        if(input.handle)handles.set(input.handle,handle); value={name:handle.name,active:handle.active};break;
      }
      case 'get':value=descriptor(r.get(input.name));break;
      case 'has':case 'hide':case 'reveal':case 'isHidden':case 'unregister':value=r[input.op](input.name);break;
      case 'markUnavailable':r.markUnavailable(input.diagnostic,input.aliases??[]);break;
      case 'unavailable':value=r.getUnavailable(input.name)??null;break;
      case 'inspect':value={state:r.state,list:r.list().map(descriptor),all:r.listAll().map(descriptor),unavailable:r.listUnavailableEntries()};break;
      case 'schemas':value=r.toCanonicalSchemas();break;
      case 'dispose':r.dispose();break;
      case 'handleDispose':handles.get(input.handle).dispose();break;
      case 'handle': {const h=handles.get(input.handle);value={name:h.name,active:h.active};break;}
      default:throw Error('unknown fixture operation');
    }
    return {input,output:{ok:true,value}};
  } catch(error) { return {input,output:{ok:false,error:(error as Error).message}}; }
});
assert.equal(new Set(recovery.map((row:any)=>row.input.id)).size,recovery.length);
assert.equal(new Set(names.map((row:any)=>row.input.id)).size,names.length);
assert.equal(deadlines.length,20);
await mkdir(outputParent,{recursive:true});
const output=await mkdtemp(join(outputParent,'tool-runtime-values-'));
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,recovery,names,deadlines,registry},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({output,recovery:recovery.length,names:names.length,deadlines:deadlines.length,registry:registry.length}));
