// Execute the original pure functions; no source is copied or patched on disk.
import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
assert.equal(process.version, 'v22.22.0');
assert.equal(process.versions.unicode, '17.0');
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const root = 'src/extension/hooks/';
const files = ['config/parseHooksConfig.ts', 'config/matchHook.ts', 'config/matchHookCondition.ts', 'protocol/events.ts', 'protocol/input.ts', 'protocol/output.ts', 'protocol/settings.ts'].map(p => root + p);
assert.equal(execFileSync('git', ['-C', baseline, 'diff', 'HEAD', '--', ...files], { encoding: 'utf8' }), '');
const sourceSha256 = Object.fromEntries(await Promise.all(files.map(async p => [p, createHash('sha256').update(await readFile(join(baseline, p))).digest('hex')])));
const load = (path: string) => import(pathToFileURL(join(baseline, root + path)).href);
const { parseHooksConfig } = await load('config/parseHooksConfig.ts');
const { matchHookMatcher } = await load('config/matchHook.ts');
const { matchHookCondition } = await load('config/matchHookCondition.ts');
const { PILOTDECK_HOOK_EVENTS, PILOTDECK_NOT_APPLICABLE_LEGACY_HOOK_EVENTS, isPilotDeckHookEvent } = await load('protocol/events.ts');
const { createHookInput, toLegacyHookInput } = await load('protocol/input.ts');

// Tagged values preserve undefined, infinities, UTF-16, own key order and
// explicit prototypes without mixing these transport tags with user records.
function encode(v: any): any {
  if (v === undefined) return ['undefined'];
  if (v === null) return ['null'];
  if (typeof v === 'number') return ['number', Number.isNaN(v) ? 'NaN' : v === Infinity ? 'Infinity' : v === -Infinity ? '-Infinity' : Object.is(v, -0) ? '-0' : v];
  if (typeof v === 'string' || typeof v === 'boolean') return [typeof v, v];
  if (Array.isArray(v)) return ['array', v.map(encode)];
  const proto = Object.getPrototypeOf(v);
  return ['object', Object.entries(v).map(([k, v]) => [k, encode(v)]), proto && proto !== Object.prototype ? encode(proto) : null];
}
const parses: any[] = [];
function parse(name: string, input: any, diagnostics?: number) {
  const result = parseHooksConfig(input);
  if (diagnostics !== undefined) assert.equal(result.diagnostics.length, diagnostics, name);
  parses.push({ name, input: encode(input), expected: JSON.stringify(result) });
  return result;
}
const config = (hooks: any[], extra = {}) => ({ PreToolUse: [{ hooks, ...extra }] });
parse('undefined', undefined, 0); parse('null', null, 0); parse('empty', {}, 0);
for (const [i,v] of [[], 0, false, '', 'x'].entries()) parse(`non-object-${i}`, v, 1);
for (const event of [...PILOTDECK_NOT_APPLICABLE_LEGACY_HOOK_EVENTS, 'pretooluse', 'unknown', '\ud800', '__proto__']) parse(`unsupported-${event}`, Object.fromEntries([[event, []]]), 1);
for (const event of PILOTDECK_HOOK_EVENTS) parse(`event-${event}`, { [event]: [{ hooks: [{ type: 'command', command: '' }] }] }, 0);
for (const [i,v] of [null, {}, 'x', 0, undefined].entries()) parse(`matchers-not-array-${i}`, { Stop: v }, 1);
parse('no-matchers', { Stop: [] }, 0);
for (const [i,v] of [null, [], 'x', 0].entries()) parse(`matcher-not-object-${i}`, { Stop: [v] }, 1);
for (const [i,v] of [{}, { hooks: null }, { hooks: {} }, { hooks: 'x' }].entries()) parse(`hooks-not-array-${i}`, { Stop: [v] }, 1);
assert.equal(parse('empty-hooks-retained', config([]), 0).settings.PreToolUse.length, 1);
for (const [i,v] of [null, [], false, {}, { type: null }, { type: 1 }].entries()) parse(`hook-type-missing-${i}`, config([v]), 1);
for (const type of ['command', 'prompt', 'http', 'agent']) {
  const field = type === 'http' ? 'url' : type === 'command' ? 'command' : 'prompt';
  parse(`${type}-missing-field`, config([{ type }]), 1);
  parse(`${type}-wrong-field`, config([{ type, [field]: 0 }]), 1);
  parse(`${type}-empty-field`, config([{ type, [field]: '' }]), 0);
  parse(`${type}-full`, config([{ type, [field]: 'fixture', if: 'Read(*)', statusMessage: 'working', once: false, timeout: -0.25, model: 'model', shell: 'powershell', async: true, asyncRewake: false, headers: { A: 'a', empty: '', number: 1, null: null }, allowedEnvVars: ['PATH', 5, '', 'PATH'] }], { matcher: 'Read|Write', pluginName: '', pluginId: 'id', pluginRoot: '/fixture/plugin' }), 0);
}
parse('callback-forbidden', config([{ type: 'callback', name: 'run' }]), 1);
parse('unsupported-type', config([{ type: 'other' }, { type: '' }, { type: '\ud800' }]), 3);
for (const timeout of [NaN, Infinity, -Infinity, -0, 0, -1, 1.5, 1e21, '2', null]) parse(`timeout-${String(timeout)}-${Object.is(timeout,-0)}`, config([{ type: 'command', command: 'x', timeout }]), 0);
for (const shell of ['bash', 'powershell', 'BASH', 'sh', '', false]) parse(`shell-${shell}`, config([{ type: 'command', command: 'x', shell }]), 0);
parse('wrong-optional-types', config([{ type: 'prompt', prompt: 'x', if: 4, statusMessage: false, once: 1, timeout: '1', model: 5 }], { matcher: 1, pluginName: null, pluginId: [], pluginRoot: {} }), 0);
parse('wrong-command-bools', config([{ type: 'command', command: 'x', async: 'true', asyncRewake: 0, once: null }]), 0);
parse('http-wrong-containers', config([{ type: 'http', url: 'u', headers: [], allowedEnvVars: {} }]), 0);
parse('http-special-keys', config([{ type: 'http', url: '', headers: JSON.parse('{"__proto__":"safe","2":"two","1":"one","z":"last"}'), allowedEnvVars: [true, null, 'a', undefined] }]), 0);
parse('event-index-boundary', JSON.parse('{"4294967295":[],"z":[],"4294967294":[]}'), 3);
parse('header-index-boundary', config([{ type: 'http', url: '', headers: JSON.parse('{"4294967295":"ordinary","z":"last","4294967294":"index"}') }]), 0);
parse('inherited-fields', { Stop: [Object.assign(Object.create({ hooks: [Object.assign(Object.create({ type: 'command', command: 'inherited', once: true }), { timeout: 2 })], matcher: '*' }), { pluginName: 'own' })] }, 0);
parse('own-events-only', Object.assign(Object.create({ Stop: [{}] }), { SessionStart: [{ hooks: [] }] }), 0);
const mixed = parse('diagnostic-order', JSON.parse('{"Z":[],"10":[],"2":[],"Stop":[null,{}, {"hooks":[null,{"type":"command"},{"type":"prompt"},{"type":"http"},{"type":"agent"},{"type":"callback"},{"type":"other"},{"type":"command","command":"ok"}]}],"SessionEnd":null}'), 13);
assert.ok(mixed.diagnostics[0].includes('event 2.'));
assert.equal(mixed.settings.Stop[0].hooks[0].command, 'ok');
// Every distinct diagnostic path must have executed.
for (const marker of ['Hooks config must', 'Unsupported hook event', 'must contain an array', 'must be an object', 'must contain hooks array', 'must contain a type', 'Command hook', 'Prompt hook', 'HTTP hook', 'Agent hook', 'runtime-only', 'Unsupported hook type']) assert.ok(parses.some(c => c.expected.includes(marker)), marker);

const matchers: any[] = [];
const match = (name: string, matcher: any, query: any) => {
  matchers.push({ name, matcher: encode(matcher), query: encode(query), expected: matchHookMatcher(matcher, query) });
};
for (const matcher of [undefined, '', '*', 'Read', 'read', ' Read ', 'Read|', '|Read', ' | Read | ', 'Read | Write', '*Read*', '/foo|bar/', '/foo/i', '//', '/', '/[/', '   ']) for (const query of [undefined, '', 'Read', 'read', 'Write', ' Read ', '/foo', 'bar/', 'foo', '*Read*', '//']) match(`literal-${matchers.length}`, matcher, query);
const regexCases: [string,string[]][] = [
  ['^Read$', ['Read','read','xRead','Read\n']], ['a+', ['','a','baa','b']], ['a+?', ['a','aa']],
  ['^a{2,3}$', ['a','aa','aaa','aaaa']], ['^a{0}$', ['a','b']], ['^.$', ['😀','é','a','\n','\r','\u2028']],
  ['^..$', ['😀','ab','a']], ['^[^]$', ['\n','\ud800','a']], ['[]', ['a',']']],
  ['[^a]', ['a','b','\n']], ['[a-z]', ['a','Z','z']], ['[z-a]', ['z']], ['[\\d-a]', ['-','a','4','q']],
  ['[a-\\d]', ['-','a','4','q']], ['\\d+', ['١','1','１２']], ['\\w+', ['é','Read_2','中']],
  ['^\\s$', ['\ufeff','\u0085','\u00a0','\u2028']], ['a.b', ['a\nb','a\rb','a\u2028b','axb']],
  ['\\bRead\\b', ['Read','xRead','Read_','éRead']], ['\\BRead', ['xRead','Read']],
  ['(a)\\1', ['aa','ab','a']], ['\\1(a)', ['a','aa']], ['^(a)?\\1$', ['a','aa','b']],
  ['^(a(b)?)+\\2$', ['aba','abab','abb']], ['(?=a)a', ['a','b']], ['(?!a)b', ['b','ab','a']],
  ['^(?=(a+))a+b\\1$', ['aaabaa','aaabaaa']], ['^(?!(a))\\1b$', ['b','ab']],
  ['(?<=ab)c', ['abc','ac','bc']], ['(?<!a)b', ['ab','cb','b']],
  ['(?<=([ab]+)([bc]+))\\1$', ['abca','abcab']], ['(?<=(a)\\1)$', ['a','aa']], ['(?<=\\1(a))$', ['a','aa']],
  ['(?<name>a)\\k<name>', ['aa','ab']], ['(?<é>a)\\k<é>', ['aa']], ['(?<\\u0061>a)\\k<a>', ['aa']],
  ['(?<x>a)\\k<y>', ['a']], ['(?<x>a)(?<x>b)', ['ab']], ['\\k<x>', ['k<x>']],
  ['\\q', ['q','\\q']], ['\\p{L}', ['p{L}','a']], ['\\xZZ', ['xZZ']], ['\\uZZZZ', ['uZZZZ']],
  ['\\u{61}', ['u'.repeat(61),'a']], ['\\8', ['8']], ['\\9', ['9']], ['\\11', ['\t']], ['\\18', ['\x018']], ['\\08', ['\x008']],
  ['\\0123', ['\n3']], ['\\400', [' 0']], ['\\777', ['?7']], ['\\cA', ['\x01']], ['\\c1', ['\\c1','c1']],
  ['[\\c1]', ['\x11','1']], ['[\\c_]', ['\x1f']], ['[\\b]', ['\b']], ['[\\B]', ['B']],
  ['a{', ['a{']], ['a}', ['a}']], ['a{1', ['a{1']], ['a{1,', ['a{1,']], ['a{z}', ['a{z}']],
  ['a{1,0}', ['a']], ['{1}', ['{1}']], ['a++', ['aa']], ['(?=a)?a', ['a']], ['(?<=a)?b', ['ab']],
  ['(?i)a', ['a','A']], ['(?s:a)', ['a']], ['(a', ['a']], ['a)', ['a']], ['\\', ['\\']],
  ['^[a\\-c]+$', ['a-c','b']], ['^[/]$', ['/']], ['^\\uD83D\\uDE00$', ['😀']], ['^\\uD800$', ['\ud800']],
];
for (const [pattern, queries] of regexCases) for (const query of queries) match(`regexp-${matchers.length}`, '/' + pattern + '/', query);
const reviewCases: [string, string[]][] = [
  ['\\|', ['|', '\\|']], ['[a-\\d-z]', ['a', '-', '4', 'z', 'q']],
  ['[\\c]', ['\\', 'c', '\x03', 'a']], ['[\\cA-\\cB]', ['\x01', '\x02', 'A']],
  ['(?<a1>a)\\k<a1>', ['aa', 'a']], ['(?<$>a)\\k<$>', ['aa', 'a']],
  ['(?<_>a)\\k<_>', ['aa', 'a']], ['(?<a\\u200Cb>c)', ['c', 'a']],
  ['(?<!a?)b', ['b', 'ab', 'cb']],
  ['[]]', [']', 'a']], ['[^]]', ['a]', ']]', 'a']], ['[]a]', ['a]', 'a']], ['[[]]', ['[]', '[']],
  ['^(?=(a))*b\\1$', ['b', 'ab']], ['^(?=(a)){2}b\\1$', ['b', 'ab']],
  ['^(?:(a)?b?)+\\1$', ['b', 'ab', 'aba']],
];
for (const [pattern, queries] of reviewCases) for (const query of queries) match(`review-quirk-${matchers.length}`, '/' + pattern + '/', query);
for (const pattern of ['(){2147483648,2147483647}', '(){4294967295,2147483647}', '(){1000000000}', '(?=a){1000000000}', '(?<x>a)[\\k]', '(?<x>a)[\\k<x>]']) match(`edge-${matchers.length}`, '/' + pattern + '/', 'ak');
match('deep-groups', '/' + '(?:'.repeat(1000) + 'a' + ')'.repeat(1000) + '/', 'a');
match('deep-lookarounds', '/' + '(?='.repeat(1000) + 'a' + ')'.repeat(1000) + '/', 'a');
for (const pattern of ['^(a*?){2}b\\1$', '^(a??){2}b\\1$', '^(a*?){3}b\\1$']) match(`nullable-capture-${matchers.length}`, '/' + pattern + '/', 'aba');
for (const char of ['\u{10940}','\u{11DB0}','\u{1E6C0}']) match(`unicode17-${matchers.length}`, `/(?<${char}>a)\\k<${char}>/`, 'aa');
for (const count of [2,3,5,10]) for (const body of ['(a?)','(a??)','(a*)','(a*?)','((?=a)a?)']) for (const query of ['b','aba','aaba','abaa']) match(`empty-prefix-${matchers.length}`, '/^' + body + '{' + count + '}b\\1$/',query);
for (const body of ['(a?)','(a??)','(a*)','(a*?)']) match(`empty-million-${matchers.length}`, '/^' + body + '{1000000}b$/', 'b');
for (const name of ['a\u1acf','\u1acf','\\u{10940}','\\uD802\\uDD40']) match(`unicode17-name-${matchers.length}`, `/(?<${name}>a)/`, 'a');
match('self-ref-billion', '/(\\1){1000000000}/','a');
// Resource budgets were chosen from probe-resources.mts before Rust changes.
// Preserve captures in the <=1M region; include a 2M noncapturing repetition.
for (const body of ['(a?)', '(a??)', '(a*)', '(a*?)']) {
  for (const query of ['b', 'ab', 'aab', 'ba']) match(`budget-capture-${matchers.length}`, `/^${body}{1000000}b\\1$/`, query);
  match(`budget-exhausted-${matchers.length}`, `/^${body}{10000000}b\\1$/`, 'b');
}
for (const pattern of ['^(?:a?){2000000}b$', '^((a?)?){500000}b$', '^(?:(a?)(b?)){500000}b$', '^(a?a?a?){500000}b$', '^((a?){1000}){999}b$', '(?<=^(a?){1000000}b)\\1$']) match(`budget-within-${matchers.length}`, `/${pattern}/`, 'b');
for (const pattern of ['^((a?){1000}){10000}b$', '(?<=^(a?){10000000}b)\\1$', '^(?!(a?){10000000}c)b$', '(?=(a?){10000000}b)b', '^(a*?){1000000000}b\\1$']) match(`budget-far-${matchers.length}`, `/${pattern}/`, 'b');
match('budget-far-negative-behind', '/(?<!^(a?){10000000}b)c/', 'bc');
match('budget-assertion-short-circuit', '/(?<!^(a?){10000000}c)b$/', 'b');
for (const body of ['(()()()()()()()()a?)', '((?=(b)(b?)(b?)(b?)(b?)(b?)(b?)(b?))a?)']) {
  match(`budget-within-captures-${matchers.length}`, `/^${body}{200000}b$/`, 'b');
  match(`budget-far-captures-${matchers.length}`, `/^${body}{1000000}b$/`, 'b');
}
match('budget-static-inner-billion', '/^((?:){1000000000}a?){1000000}b$/', 'b');
match('budget-static-captures-billion', '/^(()()()()()()()()){1000000000}b$/', 'b');
match('budget-static-look-billion', '/^(?=(b)(b?)(b?)(b?)(b?)(b?)(b?)(b?)){1000000000}b$/', 'b');
for (const row of matchers.filter(c => c.name.startsWith('budget-within') || c.name === 'budget-static-inner-billion')) assert.equal(row.expected, true, row.name);
for (const row of matchers.filter(c => c.name.startsWith('budget-far') || c.name.startsWith('budget-exhausted'))) assert.equal(row.expected, false, row.name);
// A deterministic composition matrix exercises backtracking across groups and
// quantified empty captures rather than relying only on handpicked examples.
for (const atom of ['a', '(a)', '(a?)', '(a*)', '(?:ab)', '[ab]', '(?=a)', '(?!b)']) for (const quantifier of ['', '?', '*', '+', '{0,2}', '{1,2}', '*?', '+?']) for (const tail of ['', 'a', 'b', '\\1']) for (const query of ['a', 'b', 'aa', 'ab', 'aba']) match(`composition-${matchers.length}`, '/^' + atom + quantifier + tail + '$/', query);
assert.equal(matchHookMatcher('/foo|bar/', 'foo'), false);
assert.equal(matchHookMatcher('/(a)\\1/', 'aa'), true);
assert.equal(matchHookMatcher('/^.$/', '😀'), false);

// Fixed-seed grammar samples combine classes, captures and assertions. Bounded
// quantifiers and short queries keep the real oracle generation deterministic.
let seed = 0x673c0de;
const choose = <T,>(items: T[]): T => { seed = (Math.imul(seed,1664525) + 1013904223) >>> 0; return items[seed % items.length]; };
const atoms = ['a','b','.', '[ab]', '[^a]', '\\d', '\\w', '\\s', '\\b', '(a)', '(b?)', '(a*)', '(?=a)', '(?!b)', '(?<=a)', '(?<!b)', '\\1', '\\2'];
for (let i = 0; i < 2000; i++) {
  const unit = () => choose(atoms) + choose(['','?','{0,2}','{1,2}','??']);
  const pattern = choose(['^','']) + unit() + unit() + choose(['$','']);
  for (const query of ['a','b','ab','aba']) match(`seeded-${i}-${query}`, '/' + pattern + '/',query);
}
const conditions: any[] = [];
function condition(name: string, cond: any, input: any, encodedInput = encode(input)) {
  const entry = { name, condition: encode(cond), input: encodedInput };
  try { conditions.push({ ...entry, expected: matchHookCondition(cond,input) }); }
  catch (error: any) { conditions.push({ ...entry, error: { name: error.name, message: error.message } }); }
}
for (const cond of [undefined,'','*',' ','Read','read','READ','Read(*)','Read(**)',' Read(foo) ','Read(f*oo)','Read(foo bar)','Read()','Read( )','Read(foo\nbar)','Read(foo\rbar)','Read(foo\u2028bar)','Read((foo))','Read(foo)tail','foo bar','Read[]','..','中','tool.name:v-1']) for (const toolName of [undefined,'','Read','read','R_e-a.d','Write','🙂']) condition(`condition-${conditions.length}`, cond, { toolName, toolInput: { value: 'foo bar', nested: '(foo)' } });
for (const [pattern, toolInput] of [['{}',undefined], ['{}',null], ['false',false], ['0',-0], ['1e+21',1e21], ['1e-7',1e-7], ['"1":"one","2":"two"',{ '2':'two','1':'one' }], ['"b":1,"a":2',{ b:1,a:2 }], ['\\n','\n'], ['\\ud800','\ud800'], ['"z"', { x: undefined, z: NaN }], ['null',[undefined,Infinity]] ] as any[]) condition(`json-${conditions.length}`, `Read(${pattern})`, { toolName: 'Read', toolInput });
condition('BOM-trim', '\ufeffRead(*)\ufeff', {}); condition('NEL-not-trim', '\u0085Read(*)', {});
// Store JSON text, not an encoded recursive tree: the oracle transport itself
// must not overflow while recording the real condition's RangeError.
for (const kind of ['object', 'array', 'mixed']) {
  for (const depth of [4000, 10000]) {
    const shells = Array.from({ length: depth }, (_, i) => kind === 'array' || kind === 'mixed' && i % 2 === 0 ? ['[', ']'] : ['{"x":', '}']);
    const deepRaw = shells.map(s => s[0]).join('') + '"needle"' + shells.reverse().map(s => s[1]).join('');
    const raw = '{"toolName":"Read","toolInput":' + deepRaw + '}';
    const value = JSON.parse(raw);
    condition(`depth-${kind}-${depth}`, 'Read(needle)', value, ['json', raw]);
    if (depth === 10000 && kind === 'object') {
      for (const cond of [undefined, 'Read', 'Read(*)', 'Write(needle)', 'Read(**)']) condition(`depth-short-circuit-${String(cond)}`, cond, value, ['json', raw]);
      const inherited = { toolName: 'Read', toolInput: Object.create(value.toolInput) };
      condition('depth-prototype-not-serialized', 'Read({})', inherited, ['object', [['toolName', encode('Read')], ['toolInput', ['object', [], ['json', deepRaw]]]], null]);
    }
  }
}
assert.equal(matchHookCondition('Read(**)', { toolName: '' }), true);
assert.equal(matchHookCondition('Read(f*o)', { toolName: 'Read', toolInput: 'f--o' }), false);
const inputs: any[] = [];
function input(name: string, event: string, base: any, payload: any) {
  const created = createHookInput(event, base, payload);
  inputs.push({ name, event: encode(event), base: encode(base), payload: encode(payload), expected: JSON.stringify(created), legacy: JSON.stringify(toLegacyHookInput(created)) });
}
input('minimal','Stop',{ sessionId:'s',transcriptPath:'t',cwd:'/fixture' },{});
input('overrides','PreToolUse',{ sessionId:'old',transcriptPath:'t',cwd:'old',agentId:'old',permissionMode:'ask' },{ sessionId:'new', cwd:'new',hookEventName:'ignored',agentId:null,agentType:'worker',permissionMode:undefined,toolName:'Read',toolInput:{ nestedCamel:true },HTTPStatus:200 });
input('collision','FileChanged',{ sessionId:'s',transcriptPath:'t',cwd:'c' },JSON.parse('{"session_id":"shadow","hook_event_name":"shadow","toolName":"first","tool_name":"second","__proto__":{"polluted":true},"UPPER":"x","éA":1,"\\ud800A":2}'));
input('null-optionals','SessionEnd',{ sessionId:'s',transcriptPath:'t',cwd:'c',permissionMode:null,agentId:null,agentType:null },{});
input('undefined-optionals','Stop',{ sessionId:'s',transcriptPath:'t',cwd:'c',permissionMode:undefined,agentId:undefined,agentType:undefined },{});
input('integer-keys','Stop',{ sessionId:'s',transcriptPath:'t',cwd:'c' },{ '9':'nine','1':'one',zZ:1,zz:2 });
input('index-boundary','Stop',{ sessionId:'s',transcriptPath:'t',cwd:'c' },JSON.parse('{"4294967295":"ordinary","z":"last","4294967294":"index"}'));
input('proto-primitive','Stop',{ sessionId:'s',transcriptPath:'t',cwd:'c' },JSON.parse('{"__proto__":"no-own-property","toolName":"Read"}'));
const events = [...PILOTDECK_HOOK_EVENTS,...PILOTDECK_NOT_APPLICABLE_LEGACY_HOOK_EVENTS,'pretooluse','','unknown','\ud800'].map(value => ({ value: encode(value), expected: isPilotDeckHookEvent(value) }));
const oracle = { sourceCommit, sourceSha256, nodeVersion: process.version, unicodeVersion: process.versions.unicode, parses, matchers, conditions, inputs, events };
const text = JSON.stringify(oracle,null,2) + '\n';
assert.ok(!text.includes(baseline)); assert.ok(!text.includes(output));
await mkdir(output,{ recursive:true }); await writeFile(join(output,'oracle.json'),text);
console.log(`PASS: ${parses.length} parse, ${matchers.length} matcher, ${conditions.length} condition, ${inputs.length} input, ${events.length} event cases`);
