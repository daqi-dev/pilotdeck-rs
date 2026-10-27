// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Records the baseline's real `jsonrepair` (node_modules, as imported by
// providers/anthropic/stream.ts) and `normalizeAnthropicStreamEvent` on a
// corpus of broken tool-argument JSON.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const gitExecutable = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
const ENTRY = 'src/model/providers/anthropic/stream.ts';
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [ENTRY]);
// The exact module stream.ts resolves for its bare `jsonrepair` import.
const resolved = createRequire(join(baseline, ENTRY)).resolve('jsonrepair');
const packageDir = resolve(resolved, '../../..');
const pkg = JSON.parse(readFileSync(join(packageDir, 'package.json'), 'utf8'));
const packageSha256 = Object.fromEntries(['lib/esm/regular/jsonrepair.js', 'lib/esm/utils/stringUtils.js', 'lib/cjs/regular/jsonrepair.js', 'lib/cjs/utils/stringUtils.js']
  .map((file) => [file, createHash('sha256').update(readFileSync(join(packageDir, file))).digest('hex')]));
const { jsonrepair } = await import(pathToFileURL(join(packageDir, 'lib/esm/index.js')).href);
const { normalizeAnthropicStreamEvent, createAnthropicStreamState } = await import(pathToFileURL(join(baseline, ENTRY)).href);
console.warn = () => {};
console.error = () => {};

const inputs: string[] = [
  // already valid / trivial
  '{"a":1}', '[]', '"x"', '42', ' {"a": [1, 2.5e3, -0.1]} ',
  // truncation (the streaming case)
  '{"q":"ab', '{"q":"ab\\', '{"q":', '{"q"', '{"q', '{', '[1,2', '[1,2,', '{"a":{"b":[1,{"c":"d', '{"n":-', '{"n":1.', '{"n":1e', '{"n":1e+',
  '{"u":"\\u12', '{"u":"\\u12"}', '{"u":"\\uZZZZ"}', '{"t":tru', '{"t":nul',
  // trailing / missing commas
  '{"a":1,}', '[1,2,]', '{"a":1 "b":2}', '[1 2 3]', '{,"a":1}', '[,1]',
  // quotes
  "{'a':'b'}", '{“a”:“b”}', "{‘a’:‘b’}", '{"a":"he said "hi" ok"}', '{"a":"b,c,"d":"e"}', '{"a":`x`}',
  // unquoted keys / values
  '{a:1}', '{a:b}', '{a: hello world}', '{"a":undefined}', '{"a":True,"b":False,"c":None}', '{"k":hello"}',
  // comments / whitespace
  '{"a":1 /* c */}', '{"a":1} // tail', '{ "a":1}', '{"a":　 2}', '/* only */',
  // brackets
  '{"a":1}}', '[1]]', '{"a":[1,2}', '[{"a":1]',
  // strings
  '{"a":"line\nbreak"}', '{"a":"tab\there"}', '{"a":"x\\qy"}', '{"a":"\\\nb"}', '"a" + "b"', '{"a":"x" + "y" + }',
  '{"a":"\u0001"}', '{"s":"\ud800"}', '{"u":"https://example.com/a?b=1"}', '{"u":https://example.com/x}',
  // numbers
  '{"z":00789}', '{"z":-01}', '{"z":.5}', '{"z":1.2.3}', '{"z":2e}',
  // special constructs
  '```json\n{"a":1}\n```', '[```{"a":1}```]', 'callback({"a":1});', 'NumberLong("2")', '{"r":/ab+c/}', '[1,2,...]', '{"a":1, ...}',
  '{"a":1},', '{"a":1} , ', '[1],\n', '{"a":1}\n{"b":2}', '{"a":1},\n{"b":2}', '\\"{\\"a\\":1}\\"',
  // failures
  '', '   ', ']', '{"a" 1}', '{"a":1} x', '{:1}', '{"a"::1}', '@',
];

const cases = inputs.map((input) => {
  let repaired: string | undefined;
  let error: string | undefined;
  try { repaired = jsonrepair(input); } catch (e: any) { error = e?.name ?? 'Error'; }
  let parsed: unknown;
  let parses: boolean | undefined;
  if (repaired !== undefined) {
    try { parsed = JSON.parse(repaired); parses = true; } catch { parses = false; }
  }
  // The real stream normalizer over a tool call whose argument is `input`.
  const state = createAnthropicStreamState();
  const events = [
    { type: 'content_block_start', index: 3, content_block: { type: 'tool_use', id: 'toolu_r', name: 'fix', input: {} } },
    { type: 'content_block_delta', index: 3, delta: { type: 'input_json_delta', partial_json: input } },
    { type: 'content_block_stop', index: 3 },
    { type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { output_tokens: 1 } },
  ].flatMap((raw) => normalizeAnthropicStreamEvent(raw, state));
  return { input, repaired, error, parses, parsed: parses ? parsed : undefined, events };
});
assert.equal(cases.length, 86);
const oracle = { sourceCommit, sourceSha256, node: process.version, jsonrepair: pkg.version, packageSha256, locale, cases };
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n');
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length,
  repaired: cases.filter((c) => c.parses).length, failed: cases.filter((c) => !c.parses).length }));
