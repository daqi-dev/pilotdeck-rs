// Real fixed TS FailureGuard/classify; no copied implementation or expected output.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/agent/policy/FailureGuard.ts', 'src/agent/policy/classify.ts',
]);
const { FailureGuard, buildGuardTripMessage } = await import(pathToFileURL(join(baseline, 'src/agent/policy/FailureGuard.ts')).href);
const { resolveToolUnit, toolLabel, isCountedToolFailure } = await import(pathToFileURL(join(baseline, 'src/agent/policy/classify.ts')).href);
const config = { enabled: true, modelFailureLimit: 3, toolFailureLimits: {
  read_file: 3, mcp__: 4, mcp__db__: 2, mcp__db__exact: 1, disabled: 0, negative: -1, '__proto__': 9,
}, toolLabels: { mcp__db__: '数据库𝄞', read_file: '', mcp__db__exact: '\ud800' } };
// JSON.parse keeps __proto__ as an own data property (object literal does not).
Object.defineProperty(config.toolFailureLimits, '__proto__', { value: 2, enumerable: true });
const failure = (toolName: string, code = 'tool_execution_failed', type = 'tool_result') => ({ type, result: { type: 'error', toolName, error: { code, message: 'failure' } } });
const model = { type: 'turn_continued', reason: 'model_error' };
const tool = (name: string, ok: boolean, code?: string) => ({ op: 'tool', name, ok, ...(code === undefined ? {} : { code }) });
const observe = (event: any) => ({ op: 'observe', event });
const definitions = [
  { name: 'accumulate-success-does-not-reset-and-trip-latches', actions: [observe(failure('read_file')), observe({ type: 'tool_result', result: { type: 'success', toolName: 'read_file' } }), observe(failure('read_file', 'tool_unavailable')), observe({ type: 'agent_status', event: 'other' }), observe(failure('read_file', 'setup_required')), observe(model), observe(failure('mcp__db__exact')), { op: 'reset' }, observe(failure('read_file'))] },
  { name: 'model-window-and-reset', actions: [observe(model), observe({ type: 'turn_continued', reason: 'tool_error' }), observe(model), observe(model), { op: 'model' }, { op: 'reset' }, observe(model), observe(model)] },
  { name: 'longest-prefix-shared-across-subagent', actions: [observe(failure('mcp__db__one')), observe(failure('mcp__other')), observe(failure('mcp__db__two', 'setup_required', 'subagent_tool_result')), observe(failure('mcp__db__one'))] },
  { name: 'exact-wins-and-utf16-label', actions: [observe(failure('mcp__db__exact'))] },
  { name: 'disabled-limits-and-missing-unit', actions: Array.from({ length: 5 }, () => [observe(failure('disabled')), observe(failure('negative')), observe(failure('unknown'))]).flat() },
  { name: 'tool-call-boundaries', actions: [tool('read_file', true, 'setup_required'), tool('read_file', false), tool('read_file', false, ''), tool('read_file', false, 'TOOL_EXECUTION_FAILED'), tool('read_file', false, 'permission_denied'), tool('read_file', false, 'tool_execution_failed'), tool('read_file', false, 'tool_unavailable'), tool('read_file', false, 'setup_required')] },
  { name: 'own-prototype-key', actions: [observe(failure('__proto__')), observe(failure('__proto__'))] },
  { name: 'model-limit-disabled', config: { ...config, modelFailureLimit: 0 }, actions: Array.from({ length: 6 }, () => observe(model)) },
  { name: 'independent-model-tool-counters', actions: [observe(model), observe(failure('read_file')), observe(model), observe(failure('read_file')), observe(model)] },
];
const cases = definitions.map(def => {
  const selectedConfig = def.config ?? config;
  let guard = new FailureGuard(selectedConfig);
  const results = def.actions.map((action: any) => {
    let trip;
    if (action.op === 'reset') guard = new FailureGuard(selectedConfig);
    else if (action.op === 'model') trip = guard.onModelFailure();
    else if (action.op === 'tool') trip = guard.onToolFinished(action.name, action.ok, action.code);
    else trip = guard.observe(action.event);
    return { trip: trip ?? null, isTripped: guard.isTripped, messages: trip ? [undefined, {}, { provider: 'p', model: 'm' }, { provider: '', model: '' }, { provider: 'p' }, { model: '\ud800' }].map(model => buildGuardTripMessage(trip, model)) : [] };
  });
  return { name: def.name, config: selectedConfig, actions: def.actions, results };
});
const classify = ['read_file', 'mcp__db__exact', 'mcp__db__one', 'mcp__other', 'mcp_', 'unknown', '__proto__', 'toString', 'disabled'].map(name => ({ name, resolved: resolveToolUnit(name, config.toolFailureLimits) ?? null, label: toolLabel(name, config.toolLabels) }));
const counted = ['tool_execution_failed', 'tool_unavailable', 'setup_required', '', 'permission_denied', 'tool_execution_failed ', 'TOOL_UNAVAILABLE'].flatMap(code => ['error', 'success'].map(type => {
  const result = { type, error: { code } }; return { result, counted: isCountedToolFailure(result) };
}));
const out = resolve(process.argv[2]); await mkdir(out, { recursive: true });
await writeFile(join(out, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, config, cases, classify, counted }, null, 2) + '\n');
console.log(`${cases.length} cases, ${cases.reduce((n, c) => n + c.actions.length, 0)} actions`);
