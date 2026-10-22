import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/context/compaction/CompactionOrchestrator.ts',
 'src/context/compaction/CompactionPort.ts', 'src/context/DefaultContextRuntime.ts',
 'src/context/budget/TokenBudgetManager.ts', 'src/context/budget/effectiveContext.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const load = (p: string) => import(pathToFileURL(join(baseline, p)).href);
const { createCompactionOrchestrator } = await load(sources[0]);
const { projectCompactionBudget } = await load(sources[1]);
const { DefaultContextRuntime } = await load(sources[2]);
const { TokenBudgetManager } = await load(sources[3]);
const inputs: any[] = [ {}, {maxContextTokens:null}, {maxContextTokens:'20'},
 {maxContextTokens:false}, {maxContextTokens:0}, {maxContextTokens:{}},
 {reservedOutputTokens:'3'}, {reservedOutputTokens:false}, {reservedOutputTokens:{}},
 ...[undefined, null, {}, 4, false, '', 'abc'].map(messages => ({messages})) ];
const cases = [];
for (const patch of inputs) for (const mode of ['disabled', 'native', 'evaluator']) {
 const input = {messages:[], ...patch};
 const budget = new TokenBudgetManager();
 const port = mode === 'disabled' ? {} : {budget,
  policy: {evaluateSnapshot: (snapshot: any) => ({type:'skip', snapshot})}};
 const run = createCompactionOrchestrator(port as any, {maxContextTokens:100});
 const actual = mode === 'evaluator' ? {...input, budgetEvaluator: async (messages:any) =>
  ({tokens:0,maxContextTokens:100,ratio:0,state:'ok',observed:messages})} : input;
 let expected;
 try { expected = {result:await run(actual as any)}; }
 catch(error:any) { expected = {error:error.message}; }
 cases.push({mode, input, expected});
}
const fallbackCases = [];
const runtime = new DefaultContextRuntime({maxContextTokens:100});
for (const patch of inputs.slice(0,6)) {
 const input={messages:[],...patch};
 fallbackCases.push({input,expected:await runtime.tryAutoCompact(input as any)});
}
runtime.dispose();
const summaryCases = [];
for (const value of ['40', false, null, {}, 60]) {
 const snapshot = {tokens:95, maxContextTokens:100,effectiveContextTokens:value,ratio:.95,state:'blocking'};
 let expected;
 const run = createCompactionOrchestrator({budget:{evaluate:()=>snapshot},
  policy:{evaluateSnapshot:()=>({type:'trigger',snapshot})},
  summary:{run:async (input:any)=>{expected={effective:input.effectiveContextTokens,target:input.targetPostTokens};throw Error('stop');}}
 } as any,{maxContextTokens:100});
 try { await run({messages:[]} as any); } catch(error:any) { assert.equal(error.message,'stop'); }
 summaryCases.push({snapshot, expected});
}
const projectionCases = [];
for (const input of [{}, {maxContextTokens:null}, {maxContextTokens:0},
 {allowFallbackOnFailure:true,manualForce:false,reservedOutputTokens:0},
 {budgetStage:'custom',trigger:null,budgetRequest:{provider:'p'},budgetPreparation:{tools:[]},budgetCalibration:{}},
 {budgetStage:null,trigger:'manual',budgetRequest:false,budgetPreparation:0}]) {
 projectionCases.push({input,expected:projectCompactionBudget(input as any)});
}
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline,sources), sourceSha256);
await mkdir(resolve(process.argv[2]),{recursive:true});
await writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,cases,fallbackCases,summaryCases,projectionCases},null,2)+'\n');
console.log(`recorded ${cases.length} input / ${fallbackCases.length} fallback / ${projectionCases.length} projection cases`);
