// Real TS resource probes. No executors, user settings or external IO are used.
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.equal(process.version, 'v22.22.0');
assert.equal(execFileSync('git', ['-C', process.env.PDRS_BASELINE!, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
assert.equal(execFileSync('git', ['-C', process.env.PDRS_BASELINE!, 'diff', 'HEAD', '--', 'src/extension/hooks'], { encoding: 'utf8' }), '');
const load = (p: string) => import(pathToFileURL(join(process.env.PDRS_BASELINE!, 'src/extension/hooks', p)).href);
const { matchHookMatcher } = await load('config/matchHook.ts');
const { matchHookCondition } = await load('config/matchHookCondition.ts');
const { HookRuntime } = await load('execution/HookRuntime.ts');
function atDepth<T>(depth: number, call: () => T): T {
  if (!depth) return call();
  const result = atDepth(depth - 1, call);
  assert.ok(depth > 0); // Prevent a tail-call wrapper from changing the measurement.
  return result;
}
function outcome(call: () => unknown) {
  try { return call(); } catch (e: any) { return `${e.name}: ${e.message}`; }
}
const hookInput = { sessionId: 'resource-probe', transcriptPath: '', cwd: '', hookEventName: 'PreToolUse', toolName: 'Read' };
async function runtime(matcher: string, condition: string, toolInput: unknown, depth: number) {
  // Supply all executors explicitly; this measurement cannot execute commands.
  const stub = { execute: async () => ({ stdout: '', stderr: '', exitCode: 0, outcome: 'success', output: { type: 'sync' } }) };
  const rt = new HookRuntime({ PreToolUse: [{ matcher, hooks: [{ type: 'command', command: '', if: condition }] }] }, stub, { emit() {} }, {}, stub, stub, stub, stub);
  try {
    const result = await atDepth(depth, () => rt.run({ event: 'PreToolUse', matchQuery: 'b', cwd: '', hookInput: { ...hookInput, toolInput } }));
    return result.events.length > 0;
  } catch (e: any) { return `${e.name}: ${e.message}`; }
}
console.log(JSON.stringify({ node: process.version, v8: process.versions.v8, arch: process.arch, depth: 'real HookRuntime.run + 0/32 synchronous wrapper frames' }));
// Weight = captures + quantifiers in the empty body, multiplying mandatory
// nested repetition. Literal tests cost zero; lookarounds retain capture saves.
const families = [
  { body: '(?:a?)', weight: 1 }, { body: '(a?)', weight: 2 }, { body: '(a??)', weight: 2 },
  { body: '(a*)', weight: 2 }, { body: '(a*?)', weight: 2 },
  { body: '((a?)?)', weight: 4 }, { body: '(a?)(b?)', weight: 4 },
  { body: '(a?a?a?)', weight: 4 }, { body: '((a?){1000})', weight: 2002 },
  { body: '(()()()()()()()()a?)', weight: 10 },
  { body: '((?=(b)(b?)(b?)(b?)(b?)(b?)(b?)(b?))a?)', weight: 10 },
];
for (const depth of [0, 32]) {
  for (const { body, weight } of families) {
    for (const budget of [2000000, 4000000, 8000000, 40000000]) {
      const count = Math.floor(budget / weight);
      const pattern = `^(?:${body}){${count}}b$`;
      const row = { kind: 'regexp', depth, body, count, work: count * weight,
        raw: atDepth(depth, () => outcome(() => new RegExp(pattern).test('b'))),
        ts: atDepth(depth, () => matchHookMatcher(`/${pattern}/`, 'b')),
        runtime: await runtime(`/${pattern}/`, 'Read', {}, depth) };
      console.log(JSON.stringify(row));
      if (budget === 2000000) assert.deepEqual([row.raw, row.ts, row.runtime], [true, true, true]);
      if (budget === 40000000) assert.deepEqual([row.raw, row.ts, row.runtime], ['RangeError: Maximum call stack size exceeded', false, false]);
    }
  }
  for (const kind of ['object', 'array', 'mixed']) {
    for (const count of [2000, 4000, 6000, 6500, 7000, 10000]) {
      const shells = Array.from({ length: count }, (_, i) => kind === 'array' || kind === 'mixed' && i % 2 === 0 ? ['[', ']'] : ['{"x":', '}']);
      const value = JSON.parse(shells.map(s => s[0]).join('') + '"needle"' + shells.reverse().map(s => s[1]).join(''));
      console.log(JSON.stringify({ kind: 'stringify', depth, shape: kind, count,
        ts: atDepth(depth, () => outcome(() => matchHookCondition('Read(needle)', { toolName: 'Read', toolInput: value }))),
        runtime: await runtime('*', 'Read(needle)', value, depth) }));
    }
  }
}
