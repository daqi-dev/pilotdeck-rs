// Calls the fixed TS baseline directly. Every expected value in oracle.json
// comes from these calls; nothing is synthesized on the Rust side.
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {join, resolve} from 'node:path';
import {freezeClock} from '../edgeclaw-file-memory/observe.mts';

export const NOW = '2026-10-22T00:00:00.000Z';
export const HOME = '/fixture/home';
const ROOT = 'src/context/memory/edgeclaw-memory-core/src';

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) return {$number: Object.is(v, -0) ? '-0' : String(v)};
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  return v;
}

async function transformedExtraction(baseline: string) {
  const sourcePath = join(baseline, ROOT, 'core/skills/llm-extraction.ts');
  const requireFromBaseline = createRequire(join(baseline, 'package.json'));
  const {transformSync} = createRequire(requireFromBaseline.resolve('tsx'))('esbuild');
  const exported = [
    'extractFirstJsonObject', 'extractLooseJsonEnvelope',
    'extractLooseJsonBooleanProperty', 'extractLooseJsonStringProperty',
    'tryParseLooseMemoryCreatePayload', 'normalizeStringArray', 'uniqueStrings',
    'truncate', 'normalizeWhitespace', 'truncateForPrompt',
    'normalizeMemoryRoute', 'normalizeBoolean',
  ];
  // The only import is `import type`; esbuild erases it, so the text copy of
  // the real function bodies is directly executable (same trick as the
  // edgeclaw-text fixture for service.ts).
  const code = transformSync(
    readFileSync(sourcePath, 'utf8') + `\nexport { ${exported.join(', ')} };\n`,
    {loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath},
  ).code;
  return import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
}

export async function collect(baseline: string) {
  // These helpers are deterministic, but the package convention still pins
  // ambient state before the baseline module is imported.
  process.env.HOME = HOME;
  freezeClock();
  (Math as any).random = () => 0.5;
  const ts = await transformedExtraction(baseline);
  const cases: any[] = [];
  const add = (fn: string, args: any[], run: () => any) => {
    try {
      cases.push({fn, args: enc(args), value: enc(run())});
    } catch (error: any) {
      cases.push({fn, args: enc(args), threw: error instanceof Error ? error.message : String(error)});
    }
  };
  const one = (fn: string, value: any) => add(fn, [value], () => ts[fn](value));
  const pair = (fn: string, left: any, right: any) => add(fn, [left, right], () => ts[fn](left, right));

  // extractFirstJsonObject: prose, fences, truncation, escapes, nesting,
  // multiple objects, and the brace-bounded shortcut.
  for (const raw of [
    '', '   \n\t ', '{}', '  {"a":1}  ', 'prefix prose {"a":1} suffix prose',
    '```json\n{"a":1}\n```', '```json\n{"a": {"b": 2}}\n``` tail',
    '{"a":{"b":{"c":3}}}', '{"a":1', '{"a":"}"}', '{"a":"\\"}"}',
    '{"a":"\\\\"}', 'first {"a":1} and {"b":2}', '{"a":1}{"b":2}',
    '[1,2] {"a":3}', '{ "a": [1,2,{"b":3}] }', '\uFEFF{"a":1}\uFEFF',
    '{"a":1} // trailing }', '{"a":"unterminated}', '{"a":1} x }', '}',
    '}{"a":1}', '{', '  {  "a" : 1 }  ',
    'text { "nested": "a\\"b", "arr": [ { "x": 1 } ] } more',
    '\ud800 {"a":1} \udfff', '{"a":"line\nbreak"}',
    'x {"a":1} y {"b":2}', '{"a": [1, {"b": "}"}] }',
  ]) one('extractFirstJsonObject', raw);

  // extractLooseJsonEnvelope: first `{` through last `}`, inclusive.
  for (const raw of [
    '', '   ', 'no braces here', '}{', '}', '{', 'json {"a":1} end',
    '{"a":1}{"b":2}', 'prefix {"a":"} "} tail', '```json\n{"a":1}\n```',
    '\uFEFF{"a":1}\uFEFF', 'xx {"a": [1,2]} yy }', '{{"a":1}}',
    'text {"a":1', '{"a":1} trailing } with }', '\ud800{"a":1}\udfff',
  ]) one('extractLooseJsonEnvelope', raw);

  // Loose boolean property: /i canonicalization, regex escaping, JS \s.
  for (const [raw, key] of [
    ['{"skip":true}', 'skip'],
    ['{"skip" : FALSE }', 'skip'],
    ['"SKIP":TrUe', 'skip'],
    ['{"other":true}', 'skip'],
    ['"skip": truex', 'skip'],
    ['"skip":1', 'skip'],
    ['"skip"\n:\r\n\ttrue', 'skip'],
    ['{"a.b":true}', 'a.b'],
    ['{"axb":true}', 'a.b'],
    ['"skıp":true', 'skip'],
    ['"ſkip":true', 'skip'],
    ['"skip": false, "skip": true', 'skip'],
    ['"sk$ip":true', 'sk$ip'],
    ['"sk|ip":true', 'sk|ip'],
    ['', 'skip'],
    ['"skip" :\u00a0true', 'skip'],
    ['"skip":true\uFEFF', 'skip'],
    ['"é":true', 'É'],
    ['"ſ":true', 's'],
    ['"ß":true', 'ẞ'],
    ['"ς":true', 'σ'],
    ['"Å":true', 'Å'],
  ] as [string, string][]) pair('extractLooseJsonBooleanProperty', raw, key);

  const reasonNext = ['name', 'description', 'markdown'];
  // Lazy string group with next keys.
  for (const raw of [
    '{"reason":"hello","name":"n"}',
    '{"reason":"a\\nb","name":"n"}',
    '{"reason":"he said \\"hi\\"","name":"n"}',
    '{"reason":"a,b","name":"n"}',
    '{"reason":"line1\nline2","name":"n"}',
    '{"reason":"","name":"n"}',
    '{"reason":"x","unknown":"y"}',
    '{"reason":"x","name":"n","description":"d"}',
    '{"reason":"x" , "name" : "n"}',
    '{"reason":"x", "NAME":"n"}',
    '{"reason":"x", "nameless":"n"}',
    '{"reason":"x","name":"n"',
    '{"reason":"a\\"b","name":"n"}',
    '{"reason":"a\\\\nb","name":"n"}',
    '{"reason":"a\r\nb\tc","name":"n"}',
    '{"reason":"x"\u00a0,\uFEFF"name":"n"}',
    '{"reason":"x","name":"n","name":"m"}',
  ]) add('extractLooseJsonStringProperty', [raw, 'reason', reasonNext], () => ts.extractLooseJsonStringProperty(raw, 'reason', reasonNext));
  add('extractLooseJsonStringProperty', ['{"É":"v","name":"n"}', 'é', ['name']],
    () => ts.extractLooseJsonStringProperty('{"É":"v","name":"n"}', 'é', ['name']));
  add('extractLooseJsonStringProperty', ['{"é":"v","name":"n"}', 'é', ['name']],
    () => ts.extractLooseJsonStringProperty('{"é":"v","name":"n"}', 'é', ['name']));
  // Greedy string group anchored at `}`.
  for (const raw of [
    '{"markdown":"# hi"}',
    '{"markdown":"a}b"}',
    '{"markdown":"a\\"b"}',
    '{"markdown":""}',
    '{"markdown":"x" }',
    '{"markdown":"x"}\n',
    '{"markdown":"x"} trailing',
    '{"markdown":"line1\nline2"}',
    '{"markdown":"a" } "b" }',
    '{"reason":"x","name":"n","description":"d","markdown":"m"}',
    '{"markdown":"x"  }   ',
    '{"markdown":"x"} }',
    '{"markdown":"a\\\\b"}',
  ]) add('extractLooseJsonStringProperty', [raw, 'markdown', []], () => ts.extractLooseJsonStringProperty(raw, 'markdown', []));

  // tryParseLooseMemoryCreatePayload: full/partial payloads, order, escapes.
  for (const raw of [
    '{"skip":false,"reason":"why","name":"n","description":"d","markdown":"m"}',
    '{"name":"n","description":"d","markdown":"m"}',
    '{"name":"n","description":"d"}',
    '{"description":"d","markdown":"m"}',
    '{"name":"n","markdown":"m"}',
    'prefix ```json\n{"name":"n","description":"d","markdown":"# m"}\n``` suffix',
    '{"name":"a\\"b","description":"d\\ne","markdown":"m\\t"}',
    '{"skip":"TRUE","reason":"r","name":"n","description":"d","markdown":"m"}',
    '{"skip":"no","name":"n","description":"d","markdown":"m"}',
    '{"reason":"r","name":"n","description":"d","markdown":"m","extra":1}',
    '{"name":"n","description":"d","markdown":"line1\\nline2\\\\"}',
    '', '   ', 'no json at all', '}{',
    '{"name":"n","description":"d","markdown":""}',
    '{"markdown":"m","description":"d","name":"n"}',
    '{"name":"n","description":"d","markdown":"a}b"}',
    '{"name":"n","description":"d","markdown":"a\\"b"}',
    '{"name":"n","description":"d","markdown":"m"} tail {"x":1}',
    '{"skip":true,"name":"n","description":"d","markdown":"m","reason":"r"}',
  ]) one('tryParseLooseMemoryCreatePayload', raw);

  // normalizeStringArray / uniqueStrings: JS slice(0, max) edge cases.
  for (const [value, max] of [
    ['  a  ', 3], ['', 3], ['   ', 3], ['a', 0], ['a', -1], ['a', -2],
    [' a b ', 1], [['a', 'b', 'c'], 2], [['a', 'a', 'b'], 10],
    [[' a ', ' b '], 2], [[1, 'x', null, '  ', true, 'y'], 10],
    [['a', 'b', 'c'], -1], [['a', 'b', 'c'], -2], [['a', 'b', 'c'], 0],
    [['a', 'b', 'c'], NaN], [['a', 'b', 'c'], Infinity],
    [['a', 'b', 'c'], 2.9], [['a', 'b', 'c'], 1e300], [[], 5], [[null], 5],
    [{}, 5], [true, 5], [0, 5], ['\u00a0x\u00a0', 2], [['nested'], 5],
    ['\ud800', 2], [['a'], -1.5],
  ] as [any, number][]) pair('normalizeStringArray', value, max);
  for (const [value, max] of [
    [[], 10], [['a', 'a', 'b'], 10], [[' a ', 'a ', ' b'], 10],
    [['', '  '], 10], [['a', 'b', 'c'], 2], [['a', 'b', 'c'], -1],
    [['a', 'b', 'a'], -1], [['a', 'b', 'c'], NaN],
    [['\u00a0a\u00a0', 'a'], 10], [['\ud800', '\ud800'], 10],
    [['a', 'b', 'c'], Infinity],
  ] as [string[], number][]) pair('uniqueStrings', value, max);

  // truncate / normalizeWhitespace / truncateForPrompt.
  for (const [value, max] of [
    ['', 5], ['abc', 5], ['abcdef', 3], ['abcdef', 0], ['abcdef', -2],
    ['abcdef', -10], ['  ab  ', 4], ['ab  ', 4], ['😀', 1], ['😀', 2],
    ['\ud800x', 1], ['abcdef', 2.9], ['abcdef', NaN], ['abcdef', Infinity],
    ['abcdef', -Infinity], ['abcdef', 1e300], ['a\tb\nc', 4],
    ['\u00a0ab\u00a0', 3], ['\uFEFFabc\uFEFF', 4], ['中文测试', 2],
    ['abcdef', 6], ['abcdef', 5.999],
  ] as [string, number][]) pair('truncate', value, max);
  for (const value of [
    '', '   ', 'a\t\n b', '\u00a0a\u00a0b\u00a0', '\uFEFFa\uFEFF',
    'a\u0085b', 'a\u180eb', 'a\u2028b\u2029c', '  a  b  ', '\ud800 \udfff',
    'a\n\n\nb',
  ]) one('normalizeWhitespace', value);
  for (const [value, max] of [
    ['  a   b  ', 10], ['aaa   bbb', 4], ['', 5], ['abcdef', 3],
    ['a\u00a0b', 4],
  ] as [string, number][]) pair('truncateForPrompt', value, max);

  // normalizeMemoryRoute / normalizeBoolean.
  for (const value of [
    'user', 'project', 'mix', 'none', 'USER', ' user', '', 'user ',
    true, null, 0, ['user'], {route: 'user'}, undefined,
  ]) {
    add('normalizeMemoryRoute', [value], () => ts.normalizeMemoryRoute(value));
  }
  for (const [value, fallback] of [
    [true, false], [false, true], ['true', false], ['TRUE', false],
    [' True ', false], ['false', true], ['False', true], ['1', false],
    ['yes', true], ['', true], [null, true], [undefined, false], [0, true],
    [['true'], true], [{}, true], ['TrUe', false], ['true\uFEFF', false],
    ['\u00a0true\u00a0', false], ['false ', true],
  ] as [any, boolean][]) pair('normalizeBoolean', value, fallback);

  return {cases, clockMs: Date.parse(NOW)};
}
