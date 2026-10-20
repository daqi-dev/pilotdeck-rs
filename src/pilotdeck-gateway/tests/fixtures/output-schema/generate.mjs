// Generates `ajv-oracle.json` from the real SDK 1.29.0
// `AjvJsonSchemaValidator` (Ajv 8.20 + ajv-formats 3.0.1, full formats by
// default). Run:
//
//   PDRS_BASELINE=/absolute/path/to/PilotDeck \
//     node src/pilotdeck-gateway/tests/fixtures/output-schema/generate.mjs \
//     src/pilotdeck-gateway/tests/fixtures/output-schema
//
// The generator is deterministic; a second run must be byte-identical.
import fs from 'node:fs';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';

const baseline = process.env.PDRS_BASELINE;
if (!baseline || !path.isAbsolute(baseline)) {
  throw new Error('PDRS_BASELINE must be an absolute path');
}
const outDir = process.argv[2];
if (!outDir || !path.isAbsolute(outDir)) {
  throw new Error('output directory must be an absolute path');
}
const providerPath = path.join(
  baseline,
  'node_modules/@modelcontextprotocol/sdk/dist/esm/validation/ajv-provider.js',
);
const sdkPackage = JSON.parse(fs.readFileSync(
  path.join(baseline, 'node_modules/@modelcontextprotocol/sdk/package.json'),
  'utf8',
));
const ajvPackage = JSON.parse(fs.readFileSync(
  path.join(baseline, 'node_modules/ajv/package.json'),
  'utf8',
));
const formatsPackage = JSON.parse(fs.readFileSync(
  path.join(baseline, 'node_modules/ajv-formats/package.json'),
  'utf8',
));
const hash = (relative) => createHash('sha256')
  .update(fs.readFileSync(path.join(baseline, relative)))
  .digest('hex');

assert.equal(hash('node_modules/@modelcontextprotocol/sdk/dist/esm/validation/ajv-provider.js'), '1893726b2b43d0688e8c37ce48b3c5f87a55eb93ca370433f882ae81c09c491a', 'providerSha256 drift');
assert.equal(hash('node_modules/ajv/dist/ajv.js'), 'edaed56fc546f5551b0d6dc2ff2a9cc25c2d6c83318a086eaa8c3ad0b2479297', 'ajvSha256 drift');
assert.equal(hash('node_modules/ajv-formats/dist/formats.js'), 'b61693a26869970a92cc1fe832c931ccd3afe8b6d66802b19c48306349e84880', 'formatsSha256 drift');

const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const gitExecutable = fs.realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
assert.equal(execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'],
  {env: gitEnv}).toString(), '', 'baseline tracked files must be clean');
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, ['src/mcp/client/McpClient.ts']);
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US');
assert.equal(sdkPackage.version, '1.29.0');
assert.equal(ajvPackage.version, '8.20.0');
assert.equal(formatsPackage.version, '3.0.1');

// Observe the constructor actually invoked by the SDK provider, rather than
// assuming Ajv's flags from an isolated /.../ literal.
const nativeRegExp = globalThis.RegExp;
const compiledPatterns = [];
globalThis.RegExp = new Proxy(nativeRegExp, {
  construct(target, args) {
    compiledPatterns.push({pattern: String(args[0]), flags: args[1] ?? ''});
    return Reflect.construct(target, args);
  },
});
const {AjvJsonSchemaValidator} = await import(pathToFileURL(providerPath).href);
new AjvJsonSchemaValidator().getValidator({type: 'string', pattern: '^sdk_flag_probe$'});
new AjvJsonSchemaValidator().getValidator({type: 'object', patternProperties: {'^sdk_key_probe$': {type: 'string'}}});
const regexCompilation = compiledPatterns.filter(({pattern}) => pattern.includes('sdk_'));
assert.deepEqual(regexCompilation, [
  {pattern: '^sdk_flag_probe$', flags: 'u'},
  {pattern: '^sdk_key_probe$', flags: 'u'},
]);
globalThis.RegExp = nativeRegExp;

const cases = [];
const add = (id, schema, data) => cases.push({id, schema, data});
const addCompile = (id, schema) => cases.push({id, schema, data: null});

// --- compile-time throws ---------------------------------------------------
addCompile('compile/type-unknown', {type: 'foo'});
addCompile('compile/type-array-unknown', {type: ['string', 'foo']});
addCompile('compile/type-nonstring', {type: 5});
addCompile('compile/type-null', {type: null});
addCompile('compile/type-false', {type: false});
addCompile('compile/type-zero', {type: 0});
add('type/empty-string', {type: ''}, 1);
add('type/empty-array', {type: []}, 1);
addCompile('compile/enum-empty', {enum: []});
addCompile('compile/enum-nonarray', {enum: 'a'});
addCompile('compile/ref-remote', {$ref: 'https://example.com/schema'});
addCompile('compile/ref-missing-pointer', {$ref: '#/missing'});
addCompile('compile/ref-missing-anchor', {$ref: '#foo'});
addCompile('compile/ref-nonstring', {$ref: 5});
addCompile('compile/id-nonstring', {$id: 5});
addCompile('compile/maximum-string', {maximum: '5'});
addCompile('compile/minimum-string', {minimum: '5'});
addCompile('compile/minlength-string', {type: 'string', minLength: '5'});
addCompile('compile/maxitems-string', {type: 'array', maxItems: '1'});
addCompile('compile/multipleof-string', {multipleOf: '3'});
addCompile('compile/required-nonarray', {required: 'a'});
addCompile('compile/properties-nonobject', {properties: 5});
addCompile('compile/items-bad', {items: 5});
addCompile('compile/additionalproperties-bad', {additionalProperties: 5});
addCompile('compile/not-bad', {not: 5});
addCompile('compile/allof-nonarray', {allOf: {}});
addCompile('compile/uniqueitems-nonbool', {type: 'array', uniqueItems: 'yes'});
addCompile('compile/nullable-no-type', {nullable: true});
addCompile('compile/nullable-contradicts', {type: 'null', nullable: false});
addCompile('compile/nullable-nonbool', {type: 'string', nullable: 'yes'});
cases.push({
  id: 'compile/pattern-invalid',
  schema: {type: 'string', pattern: '['},
  data: 'x',
  compileErrorPrefix: 'Invalid regular expression',
});

for (const [id, schema] of [
  ['compile/combined-rust-flags', {type: 'string', pattern: '(?im:\\w)'}],
  ['compile/rust-anchor', {type: 'string', pattern: '\\A\\w+$'}],
  ['compile/property-pattern-invalid', {patternProperties: {'[': {type: 'number'}}}],
]) cases.push({id, schema, data: null, compileErrorPrefix: 'Invalid regular expression'});

// ECMAScript `/u` syntax rejected by V8. Ajv feeds both `pattern` and every
// `patternProperties` key to `new RegExp(pattern, "u")`, so these must fail
// schema compilation instead of being accepted by Rust's more permissive
// parser. The first block is the exact set from Grok r3 P1-A; `exact` cases
// compare the whole V8 message, not just a prefix.
const invalidPatterns = [
  ['grok/invalid-escape-hyphen', '\\-', 'Invalid escape', true],
  ['grok/invalid-escape-underscore', '\\_', 'Invalid escape', true],
  ['grok/invalid-escape-hash', '\\#', 'Invalid escape', true],
  ['grok/invalid-escape-ampersand', '\\&', 'Invalid escape', true],
  ['grok/invalid-escape-tilde', '\\~', 'Invalid escape', true],
  ['grok/invalid-escape-at', '\\@', 'Invalid escape', true],
  ['grok/invalid-escape-percent', '\\%', 'Invalid escape', true],
  ['grok/invalid-escape-space', '\\ ', 'Invalid escape', true],
  ['grok/lone-close-bracket', 'a]', 'Lone quantifier brackets', true],
  ['grok/lone-close-brace', '}', 'Lone quantifier brackets', true],
  ['grok/nothing-to-repeat', 'a**', 'Nothing to repeat', true],
  ['grok/rust-word-boundary', '\\<a', 'Invalid escape', true],
  ['edge/lone-close-bracket', ']', 'Lone quantifier brackets'],
  ['edge/lone-open-brace', '{', 'Lone quantifier brackets'],
  ['edge/nothing-to-repeat-star', '*a', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-plus', '+a', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-question', '?a', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-double-plus', 'a++', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-star-plus', 'a*+', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-double-lazy', 'a*??', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-range-double-lazy', 'a{2,3}??', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-quantified-lookahead', '(?=a)*', 'Invalid quantifier'],
  ['edge/nothing-to-repeat-boundary', '\\b*', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-bare-exact', '{2}', 'Nothing to repeat'],
  ['edge/nothing-to-repeat-bare-range', '{2,3}', 'Nothing to repeat'],
  ['edge/incomplete-quantifier-open', 'a{2x}', 'Incomplete quantifier'],
  ['edge/incomplete-quantifier-no-lower', 'a{,3}', 'Incomplete quantifier'],
  ['edge/incomplete-quantifier-comma', 'a{2,', 'Incomplete quantifier'],
  ['edge/incomplete-quantifier-eof', 'a{1', 'Incomplete quantifier'],
  ['edge/incomplete-quantifier-range-eof', 'a{2,3', 'Incomplete quantifier'],
  ['edge/incomplete-quantifier-space', 'a{2, 3}', 'Incomplete quantifier'],
  ['edge/quantifier-reversed', 'a{2,1}', 'numbers out of order in {} quantifier'],
  ['edge/quantifier-after-quantifier', 'a{2}{3}', 'Nothing to repeat'],
  ['edge/invalid-decimal-escape', '\\01', 'Invalid decimal escape'],
  ['edge/backreference-ten-missing', '\\10', 'Invalid escape'],
  ['edge/invalid-class-escape', '[\\_]', 'Invalid escape'],
  ['edge/invalid-class-hash', '[\\#]', 'Invalid escape'],
  ['edge/invalid-class-space', '[\\ ]', 'Invalid escape'],
  ['edge/invalid-class-k', '[\\k]', 'Invalid escape'],
  ['edge/invalid-class-B', '[\\B]', 'Invalid escape'],
  ['edge/invalid-class-decimal', '[\\1]', 'Invalid class escape'],
  ['edge/backreference-missing', '\\1', 'Invalid escape'],
  ['edge/backreference-too-large', '(a)(b)\\10', 'Invalid escape'],
  ['edge/noncapturing-not-counted', '(?:a)\\1', 'Invalid escape'],
  ['edge/named-reference-missing', '\\k<x>', 'Invalid named capture referenced'],
  ['edge/named-reference-unknown', '(?<x>a)\\k<y>', 'Invalid named capture referenced'],
  ['edge/group-name-digit-first', '(?<1a>x)', 'Invalid capture group name'],
  ['edge/group-name-empty', '(?<>)', 'Invalid capture group name'],
  ['edge/group-name-duplicate', '(?<x>a)(?<x>b)', 'Duplicate capture group name'],
  ['edge/group-inline-flag', '(?i:a)', 'Invalid group'],
  ['edge/group-rust-name', '(?P<x>a)', 'Invalid group'],
  ['edge/rust-anchor', '\\A', 'Invalid escape'],
  ['edge/rust-anchor-z', '\\z', 'Invalid escape'],
  ['edge/rust-bell', '\\a', 'Invalid escape'],
  ['edge/rust-escape', '\\e', 'Invalid escape'],
  ['edge/rust-hex-brace', '\\x{41}', 'Invalid escape'],
  ['edge/hex-short', '\\x1', 'Invalid escape'],
  ['edge/hex-bad', '\\xG1', 'Invalid escape'],
  ['edge/unicode-too-large', '\\u{110000}', 'Invalid Unicode escape'],
  ['edge/control-bad', '\\c1', 'Invalid Unicode escape'],
  ['edge/control-bracket', '\\c[', 'Invalid Unicode escape'],
  ['edge/control-underscore', '\\c_', 'Invalid Unicode escape'],
  ['edge/control-alone', '\\c', 'Invalid Unicode escape'],
  ['edge/property-missing-braces', '\\p', 'Invalid property name'],
  ['edge/class-shorthand-range-left', '[\\w-a]', 'Invalid character class'],
  ['edge/class-shorthand-range-right', '[a-\\w]', 'Invalid character class'],
  ['edge/class-two-shorthand-range', '[\\d-\\w]', 'Invalid character class'],
  ['edge/class-range-to-close', '[a-\\]]', 'Range out of order in character class'],
  ['edge/class-range-to-hyphen', '[a-\\-]', 'Range out of order in character class'],
  ['edge/class-double-dash', '[a--b]', 'Range out of order in character class'],
  ['edge/class-reversed-astral', '[😁-😀]', 'Range out of order in character class'],
  ['edge/class-unterminated', '[a', 'Unterminated character class'],
  ['edge/class-negated-unterminated', '[^a', 'Unterminated character class'],
  ['edge/class-backslash-unterminated', '[\\]', 'Unterminated character class'],
  ['edge/group-unterminated', '(a', 'Unterminated group'],
  ['edge/noncapturing-unterminated', '(?:a', 'Unterminated group'],
  ['edge/lookahead-unterminated', '(?=a', 'Unterminated group'],
  ['edge/class-empty-then-bracket', '[]]', 'Lone quantifier brackets'],
  ['edge/class-negated-empty-then-bracket', '[^]]', 'Lone quantifier brackets'],
  ['edge/class-empty-then-literal', '[]a]', 'Lone quantifier brackets'],
  ['edge/class-negated-empty-then-literal', '[^]a]', 'Lone quantifier brackets'],
];
for (const [name, pattern, detail, exact] of invalidPatterns) {
  cases.push({
    id: `invalid-pattern/${name}`,
    schema: {type: 'string', pattern},
    data: 'x',
    compileErrorPrefix: 'Invalid regular expression',
    compileErrorDetail: detail,
    ...(exact ? {exactCompileError: true} : {}),
  });
}
cases.push({
  id: 'invalid-pattern/grok-property-pattern',
  schema: {type: 'object', patternProperties: {'\\-': {type: 'number'}}},
  data: {x: 1},
  compileErrorPrefix: 'Invalid regular expression',
  compileErrorDetail: 'Invalid escape',
  exactCompileError: true,
});

// --- nullable and anchors --------------------------------------------------
add('nullable/null', {type: 'string', nullable: true}, null);
add('nullable/number', {type: 'string', nullable: true}, 5);
add('nullable/enum-null', {type: 'string', nullable: true, enum: ['a']}, null);
add(
  'anchor/invalid',
  {
    type: 'object',
    properties: {a: {$ref: '#foo'}},
    definitions: {x: {$id: '#foo', type: 'string'}},
  },
  {a: 1},
);
add(
  'anchor/valid',
  {
    type: 'object',
    properties: {a: {$ref: '#foo'}},
    definitions: {x: {$id: '#foo', type: 'string'}},
  },
  {a: 'x'},
);
add(
  'ref/sibling-keywords',
  {$ref: '#/definitions/x', minimum: 10, definitions: {x: {type: 'number'}}},
  5,
);
add(
  'ref/recursive-499',
  {type: 'object', properties: {a: {$ref: '#'}}},
  (() => {
    let data = {};
    let cursor = data;
    for (let i = 0; i < 499; i++) {
      cursor.a = {};
      cursor = cursor.a;
    }
    return data;
  })(),
);

// --- formats (full mode) ---------------------------------------------------
for (const [id, schema, data] of [
  ['date/leap', {type: 'string', format: 'date'}, '2024-02-29'],
  ['date/not-leap', {type: 'string', format: 'date'}, '2023-02-29'],
  ['date/month', {type: 'string', format: 'date'}, '2024-13-01'],
  ['time/offset', {type: 'string', format: 'time'}, '12:00:00+02:00'],
  ['time/missing-zone', {type: 'string', format: 'time'}, '12:00:00'],
  ['time/leap-second', {type: 'string', format: 'time'}, '23:59:60Z'],
  ['iso-time/missing-zone', {type: 'string', format: 'iso-time'}, '12:00:00'],
  ['date-time/space', {type: 'string', format: 'date-time'}, '2024-02-29 12:00:00Z'],
  ['date-time/nope', {type: 'string', format: 'date-time'}, 'nope'],
  ['iso-date-time/space-no-zone', {type: 'string', format: 'iso-date-time'}, '2024-02-29 12:00:00'],
  ['duration/full', {type: 'string', format: 'duration'}, 'P1Y2M3DT4H5M6S'],
  ['duration/bare', {type: 'string', format: 'duration'}, 'P'],
  ['duration/weeks', {type: 'string', format: 'duration'}, 'P2W'],
  ['uri/ok', {type: 'string', format: 'uri'}, 'https://example.com/a'],
  ['uri/bad', {type: 'string', format: 'uri'}, 'not a uri'],
  ['uri-reference/ok', {type: 'string', format: 'uri-reference'}, '/a/b'],
  ['uri-template/ok', {type: 'string', format: 'uri-template'}, '{var}'],
  ['url/ok', {type: 'string', format: 'url'}, 'https://example.com/a?b=1'],
  ['url/private', {type: 'string', format: 'url'}, 'http://192.168.1.1/x'],
  ['email/upper', {type: 'string', format: 'email'}, 'User@Example.com'],
  ['email/double-dot', {type: 'string', format: 'email'}, 'a..b@example.com'],
  ['email/localhost', {type: 'string', format: 'email'}, 'a@localhost'],
  ['hostname/trailing-dot', {type: 'string', format: 'hostname'}, 'a-b.example.'],
  ['hostname/leading-hyphen', {type: 'string', format: 'hostname'}, '-a.example'],
  ['ipv4/leading-zero', {type: 'string', format: 'ipv4'}, '01.1.1.1'],
  ['ipv4/over', {type: 'string', format: 'ipv4'}, '256.1.1.1'],
  ['ipv6/ok', {type: 'string', format: 'ipv6'}, '::1'],
  ['ipv6/bad', {type: 'string', format: 'ipv6'}, '1.2.3.4'],
  ['uuid/urn', {type: 'string', format: 'uuid'}, 'urn:uuid:123e4567-e89b-12d3-a456-426614174000'],
  ['uuid/bad', {type: 'string', format: 'uuid'}, '123e4567e89b12d3a456426614174000'],
  ['json-pointer/ok', {type: 'string', format: 'json-pointer'}, '/a~1b'],
  ['json-pointer-uri-fragment/ok', {type: 'string', format: 'json-pointer-uri-fragment'}, '#/a'],
  ['relative-json-pointer/ok', {type: 'string', format: 'relative-json-pointer'}, '0/a'],
  ['byte/ok', {type: 'string', format: 'byte'}, 'aGVsbG8='],
  ['byte/bad', {type: 'string', format: 'byte'}, 'aGVsbG8'],
  ['regex/ok', {type: 'string', format: 'regex'}, '^a+$'],
  ['regex/bad', {type: 'string', format: 'regex'}, '['],
  ['regex/lookahead', {type: 'string', format: 'regex'}, '(?=a)'],
  ['password/any', {type: 'string', format: 'password'}, 'anything'],
  ['binary/any', {type: 'string', format: 'binary'}, 'anything'],
  ['int32/number-over', {type: 'number', format: 'int32'}, 3000000000],
  ['int32/number-ok', {type: 'number', format: 'int32'}, 42],
  ['int32/string-not-checked', {type: 'string', format: 'int32'}, '3000000000'],
  ['int64/fraction', {type: 'number', format: 'int64'}, 1.5],
  ['float/large', {type: 'number', format: 'float'}, 1.5e300],
  ['format/object-data', {type: 'object', format: 'email'}, 'x'],
]) {
  add(id, schema, data);
}

// --- JS /u character atoms (both pattern and patternProperties) ------------
for (const [name, pattern, values] of [
  ['word', '^\\w+$', ['hello_19', 'héllo', '中文', 'K']],
  ['not-word', '^\\W+$', ['é', 'A', '😀']],
  ['digit', '^\\d+$', ['123', '١٢٣', '１２３']],
  ['not-digit', '^\\D+$', ['١', '1']],
  ['space', '^\\s$', ['\ufeff', '\u0085', '\u00a0', '\u2028', '\u2029', '\t', '\n', ' ']],
  ['not-space', '^\\S$', ['\ufeff', '\u0085', 'x']],
  ['boundary', '\\bé\\b', ['é', 'aéa']],
  ['not-boundary', '\\Bé\\B', ['é', 'aéa']],
  ['dot', '^.$', ['\n', '\r', '\u2028', '\u2029', '😀', '\ufeff']],
  ['class-word', '^[\\w]+$', ['é', 'A_7']],
  ['class-not-word', '^[\\W]+$', ['é', 'A']],
  ['class-digit', '^[\\d]+$', ['١', '1']],
  ['class-not-digit', '^[\\D]+$', ['١', '1']],
  ['class-space', '^[\\s]$', ['\ufeff', '\u0085']],
  ['class-not-space', '^[\\S]$', ['\ufeff', '\u0085']],
  ['class-backspace', '^[\\b]$', ['\b', 'b']],
  ['class-dot', '^[.]$', ['.', '\r']],
  ['escaped-dot', '^\\.$', ['.', 'x']],
  ['escaped-backslash', '^\\\\w$', ['\\w', 'w']],
  ['mixed-class', '^[a\\d_]$', ['a', '_', '1', '١']],
]) {
  for (const [index, data] of values.entries()) {
    add(`pattern/${name}-${index}`, {type: 'string', pattern}, data);
    add(`patternProperties/${name}-${index}`, {
      type: 'object', patternProperties: {[pattern]: {type: 'number'}}, additionalProperties: false,
    }, {[data]: 1});
  }
}
// JS /u escapes whose meaning Rust spells differently but can still express.
for (const [id, pattern, data] of [
  ['control-j', '\\cJ', '\n'],
  ['control-j-lower', '\\cj', '\n'],
  ['nul', '\\0', '\0'],
  ['hex-latin1', '\\xFF', '\u00FF'],
  ['class-control-j', '[\\cJ]', '\n'],
  ['class-nul', '[\\0]', '\0'],
  ['class-hex', '[\\x41]', 'A'],
  ['class-escaped-hyphen', '[\\-]', '-'],
  ['class-escaped-hyphen-then-a', '[\\-a]', '-'],
  ['class-dash-range', '[\\--a]', '-'],
  ['class-range-from-close', '[\\]-a]', 'a'],
  ['class-escaped-close-bracket', '[\\]]', ']'],
  ['class-escaped-open-bracket', '[\\[]', '['],
  ['class-escaped-dot', '[\\.]', '.'],
  ['class-backslash', '[\\\\]', '\\'],
  ['quantifier-leading-zero', 'a{01}', 'aa'],
  ['triple-range', '[a-b-c]', '-'],
  ['class-only-dash', '[-]', '-'],
  ['class-star', '[*]', '*'],
  ['class-open-brace', '[{]', '{'],
  ['class-close-brace', '[}]', '}'],
  ['class-ampersand', '[a&b]', '&'],
  ['class-pipe', '[a|b]', '|'],
  ['class-caret', '[a^b]', '^'],
  ['class-quantifier-braces', '[a{2}]', '2'],
  ['class-negated-star', '[^*]', 'a'],
  ['astral-range', '[😀-😁]', '😀'],
  ['astral-single', '😀', '😀'],
  ['empty-pattern', '', ''],
  ['alternation-trailing', 'a|', ''],
  ['alternation-leading', '|a', ''],
  ['alternation-empty-group', '(a|)', ''],
  ['empty-group', '()', ''],
  ['noncapturing-empty', '(?:)', ''],
  ['lazy-star', 'a*?', 'aa'],
  ['lazy-range', 'a{2,3}?', 'aaa'],
]) {
  add(`escape/${id}`, {type: 'string', pattern}, data);
}
add('type/array-mismatch', {type: ['string', 'number']}, false);
for (const [format, name, values] of [
  ['hostname', 'label-length', ['a'.repeat(63), 'a'.repeat(64)]],
  ['hostname', 'total-length', [Array(4).fill('a'.repeat(63)).join('.'), Array(3).fill('a'.repeat(63)).concat('a'.repeat(61)).join('.')]],
  ['hostname', 'unicode', ['é.example', 'xn--9ca.example', 'a_b.example']],
  ['url', 'gate', ['https://example.com', 'ftp://example.com/a', 'http://localhost', 'http://127.0.0.1/', 'http://8.8.8.8/', 'https://example.com:65536', 'https://user:pass@example.com/a']],
  ['time', 'late-offset', ['23:59:60+00:00', '00:59:60+01:00', '23:59:60+01:00', '12:00:00+25:00']],
  ['uuid', 'case', ['URN:UUID:123E4567-E89B-12D3-A456-426614174000', '00000000-0000-0000-0000-000000000000']],
  ['ipv4', 'edge', ['0.0.0.0', '255.255.255.255', '1.2.3.4\n']],
  ['date', 'leap', ['1900-02-29', '2000-02-29', '0000-02-29']],
]) {
  values.forEach((data, index) => add(`format-edge/${format}-${name}-${index}`, {type: 'string', format}, data));
}

// Ambiguous Rust-only class operators and JavaScript-only regex constructs
// are explicitly unsupported by the port. Keep the real Ajv result as
// evidence (including invalid /u range syntax, which is a compile error and
// therefore lives in the invalid-pattern block above).
for (const [id, schema, data] of [
  ['intersection-not', {not: {pattern: '[\\w&&a]'}}, 'b'],
  ['intersection-property', {patternProperties: {'[\\w&&a]': {type: 'number'}}}, {b: 'bad'}],
  ['symmetric-difference', {pattern: '[a~~b]'}, '~'],
  ['nested-class', {pattern: '[[]'}, '['],
  ['empty-class', {pattern: '[]'}, ''],
  ['negated-empty-class', {pattern: '^[^]$'}, '\n'],
  ['double-dash-start', {pattern: '[--a]'}, '-'],
  ['lookahead', {pattern: '(?=a)'}, 'a'],
  ['lookbehind', {pattern: '(?<=a)b'}, 'ab'],
  ['backreference', {pattern: '(a)\\1'}, 'aa'],
  ['forward-backreference', {pattern: '\\1(a)'}, 'a'],
  ['forward-backreference-two', {pattern: '\\2(a)(b)'}, 'b'],
  ['backreference-two-groups', {pattern: '(a)(b)\\2'}, 'ab'],
  ['named-group', {pattern: '(?<x>a)\\k<x>'}, 'a'],
  ['named-forward-reference', {pattern: '\\k<x>(?<x>a)'}, 'a'],
  ['named-group-nonascii', {pattern: '(?<é>x)'}, 'x'],
  ['unicode-escape', {pattern: '\\u0041'}, 'A'],
  ['unicode-brace-escape', {pattern: '\\u{1F600}'}, '😀'],
  ['lone-surrogate-escape', {pattern: '\\uD83D'}, '\uD83D'],
  ['surrogate-pair-escape', {pattern: '\\uD83D\\uDE00'}, '😀'],
  ['property-escape', {pattern: '\\p{L}'}, 'a'],
  ['property-escape-in-class', {pattern: '[\\p{L}]'}, 'a'],
  ['property-name-not-validated', {pattern: '\\p{NotAProperty}'}, 'a'],
  ['property-name-not-validated-in-class', {pattern: '[\\p{NotAProperty}]'}, 'a'],
  ['huge-quantifier', {pattern: 'a{1000000000}'}, 'a'],
]) cases.push({id: `unsupported-pattern/${id}`, schema, data, portUnsupported: true});
add('length/astral-max-equal', {type: 'string', maxLength: 1}, '😀');
add('length/astral-min-too-short', {type: 'string', minLength: 2}, '😀');
add('format-edge/byte-multiline', {type: 'string', format: 'byte'}, 'aGVsbG8=\n!!');
add('format-edge/time-fractional-leap', {type: 'string', format: 'time'}, '23:59:60.5Z');

// --- uniqueItems paths -----------------------------------------------------
add('unique/untyped', {type: 'array', uniqueItems: true}, [1, 1]);
add(
  'unique/typed-items',
  {type: 'array', items: {type: 'string'}, uniqueItems: true},
  [1, 'a', 'a'],
);
add(
  'unique/multi-typed-items',
  {type: 'array', items: {type: ['string', 'number']}, uniqueItems: true},
  ['1', 1],
);

// --- required stringifies non-strings --------------------------------------
add('required/nonstring', {required: [5]}, {});

const oracle = {
  sourceCommit, sourceSha256, regexCompilation,
  sdk: sdkPackage.version,
  ajv: ajvPackage.version,
  ajvFormats: formatsPackage.version,
  providerSha256: hash('node_modules/@modelcontextprotocol/sdk/dist/esm/validation/ajv-provider.js'),
  ajvSha256: hash('node_modules/ajv/dist/ajv.js'),
  formatsSha256: hash('node_modules/ajv-formats/dist/formats.js'),
  cases: cases.map((entry) => {
    const {id, schema, data} = entry;
    const validator = new AjvJsonSchemaValidator();
    let compiled = false;
    try {
      const validate = validator.getValidator(schema);
      compiled = true;
      assert.ok(!entry.compileErrorPrefix, `${id}: expected the real SDK to reject compilation`);
      const result = validate(data);
      return result.valid
        ? {...entry}
        : {...entry, errorText: result.errorMessage};
    } catch (error) {
      if (entry.compileErrorPrefix) {
        if (compiled) throw error;
        assert.ok(error.message.startsWith(entry.compileErrorPrefix), `${id}: ${error.message}`);
        if (entry.compileErrorDetail) {
          assert.ok(
            error.message.endsWith(`: ${entry.compileErrorDetail}`),
            `${id}: ${error.message}`,
          );
        }
        return {...entry, actualCompileError: error.message};
      }
      if (error instanceof RangeError) throw error;
      return {...entry, compileError: error.message};
    }
  }),
};

const target = path.join(outDir, 'ajv-oracle.json');
fs.writeFileSync(target, `${JSON.stringify(oracle, null, 2)}\n`);
console.log(`wrote ${target}: ${oracle.cases.length} cases`);
