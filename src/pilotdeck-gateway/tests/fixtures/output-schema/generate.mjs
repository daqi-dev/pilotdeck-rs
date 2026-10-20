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
import os from 'node:os';
import {createHash} from 'node:crypto';
import {fileURLToPath, pathToFileURL} from 'node:url';

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
const addPatternCompile = (id, schema, detail) => cases.push({
  id,
  schema,
  data: null,
  compileErrorPrefix: 'Invalid regular expression',
  compileErrorDetail: detail,
  exactCompileError: true,
});

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
for (const segment of ['01', '+1']) {
  addCompile(`compile/ref-array-${segment}`, {items: [true, false], $ref: `#/items/${segment}`});
}
for (const segment of ['0', '1']) {
  add(`ref-array/canonical-${segment}`, {items: [true, false], $ref: `#/items/${segment}`}, null);
}
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
  // Property names are the ECMAScript property list, not Rust's looser
  // Unicode tables. `${...}` is a valid escape spelling but not a valid name.
  ['property/lone-unknown', '\\p{Foo}', 'Invalid property name', true],
  ['property/lone-script', '\\p{Latin}', 'Invalid property name', true],
  ['property/class-unknown', '[\\p{Foo}]', 'Invalid property name in character class', true],
  ['property/lowercase-key', '\\p{script=Han}', 'Invalid property name', true],
  ['property/unknown-key', '\\p{Foo=Bar}', 'Invalid property name', true],
  ['property/binary-with-value', '\\p{Alphabetic=Y}', 'Invalid property name', true],
  ['property/spaced', '\\p{Script = Han}', 'Invalid property name', true],
  ['group-name-euro', '(?<€>a)', 'Invalid capture group name', true],
  ['group-name-arabic-digit', '(?<١>a)', 'Invalid capture group name', true],
  ['group-name-escaped-euro', '(?<\\u{20AC}>a)', 'Invalid capture group name', true],
  ['group-name-escape-q', '(?<\\q>a)', 'Invalid capture group name', true],
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

// Common patterns the real SDK accepts. These are compiled and evaluated by
// the port (not merely accepted): a false "unsupported" here would hide real
// tools from `tools/list`, exactly the r4 regression.
for (const [name, pattern, values] of [
  ['unicode-bmp', '^\\u00e9$', ['é', 'e']],
  ['unicode-cjk-range', '^[\\u4e00-\\u9fa5]+$', ['中文', 'abc']],
  ['unicode-no-controls', '^[^\\u0000-\\u001f]*$', ['ok', 'a\u0001b']],
  ['unicode-brace', '^\\u{1F600}$', ['😀', '😁']],
  ['unicode-surrogate-pair', '^\\uD83D\\uDE00$', ['😀', 'x']],
  ['hex-escape', '^\\x41$', ['A', 'B']],
  ['control-escape', '^\\cJ$', ['\n', 'J']],
  ['nul-escape', '^\\0$', ['\0', '0']],
  ['named-groups', '^(?<year>\\d{4})-(?<month>\\d{2})$', ['2026-10', '2026-1']],
  ['named-group-single', '^(?<y>a)$', ['a', 'b']],
  ['property-letter', '^\\p{L}+$', ['abc', '123']],
  ['property-letter-class', '^[\\p{L}]+$', ['中文', '12']],
  ['property-script', '^\\p{Script=Han}+$', ['中文', 'abc']],
  ['property-categories', '^\\p{Lu}\\p{Ll}+$', ['Hello', 'hello']],
  ['word-hyphen', '^[\\w-]+$', ['a-b_1', 'a b']],
  ['word-dot-hyphen', '^[\\w.-]+$', ['a.b-c', 'a/b']],
  ['letters-space', '^[a-z\\s]+$', ['a b\tc', 'a1']],
  ['digits-space', '^[0-9\\s]+$', ['12 3', '12a']],
  ['dash-digit-left', '^[-\\d]+$', ['-12', 'a1']],
  ['dash-digit-right', '^[\\d-]+$', ['1-2', 'a1']],
  ['word-space-hyphen', '^[\\w\\s-]+$', ['a b-c', 'a.b']],
  ['escaped-hyphen-class', '^[a-zA-Z0-9_\\-]+$', ['a-b', 'a.b']],
  ['cjk-and-word', '^[\\u4e00-\\u9fa5\\w]+$', ['中文a_1', '中文 a']],
  ['non-ascii', '^[^\\u0000-\\u007F]+$', ['é中', 'abc']],
  ['emoji-range-brace', '^\\u{1F600}-\\u{1F64F}$', ['😀-🙏', 'a']],
  ['emoji-class', '^[😀-😁]$', ['😀', '😁']],
  ['max-code-point', '^\\u{10FFFF}$', [String.fromCodePoint(0x10FFFF), 'x']],
  ['noncapturing', '^(?:foo|bar)$', ['foo', 'baz']],
  ['bounded-quantifier', '^\\d{2,4}$', ['123', '1']],
  ['optional-hyphen', '^[\\-]?\\d+$', ['-42', '--42']],
  ['escaped-slash', '^\\/[a-z]+$', ['/abc', 'abc']],
  ['escaped-dot', '^\\.$', ['.', 'x']],
  ['escaped-anchors', '^\\$\\^$', ['$^', 'a']],
  ['named-word', '^(?<word>[A-Za-z]+)$', ['hello', 'hello1']],
  ['regional-indicator-range', '^\\u{1F1E6}-\\u{1F1FF}$', ['🇦-🇿', 'A']],
  ['mix-property-class', '^[\\p{Script=Han}\\p{L}]+$', ['中文abc', '123']],
  ['property-emoji', '^\\p{Emoji}$', ['😀', 'a']],
  ['named-group-unicode', '^(?<é>x)$', ['x', 'y']],
]) {
  for (const [index, data] of values.entries()) {
    add(`positive-pattern/${name}-${index}`, {type: 'string', pattern}, data);
  }
}

// `patternProperties` compiles a key only when the keyword actually runs.
// All-always-valid children make Ajv's codegen return before `usePattern`
// unless a non-trivial `additionalProperties` compiles the keys again.
add(
  'pattern-properties/always-valid-invalid-key',
  {type: 'object', patternProperties: {'\\-': {}}},
  {x: 1},
);
add(
  'pattern-properties/always-valid-js-only-key',
  {type: 'object', patternProperties: {'(?=a)': {}}},
  {x: 1},
);
add(
  'pattern-properties/always-valid-true-child',
  {type: 'object', patternProperties: {'\\-': true}},
  {x: 1},
);
add(
  'pattern-properties/always-valid-metadata-child',
  {type: 'object', patternProperties: {'\\-': {title: 'x'}}},
  {x: 1},
);
add(
  'pattern-properties/always-valid-additional-true',
  {type: 'object', patternProperties: {'\\-': {}}, additionalProperties: true},
  {x: 1},
);
add(
  'pattern-properties/always-valid-additional-empty',
  {type: 'object', patternProperties: {'\\-': {}}, additionalProperties: {}},
  {x: 1},
);
addPatternCompile(
  'pattern-properties/always-valid-additional-false',
  {type: 'object', patternProperties: {'\\-': {}}, additionalProperties: false},
  'Invalid escape',
);
addPatternCompile(
  'pattern-properties/mixed-child',
  {type: 'object', patternProperties: {'\\-': {}, x: {type: 'number'}}},
  'Invalid escape',
);
addPatternCompile(
  'pattern-properties/always-valid-additional-schema',
  {type: 'object', patternProperties: {'\\-': {}}, additionalProperties: {type: 'number'}},
  'Invalid escape',
);

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

// `ajv-formats` `regex` is `new RegExp(str)` without `/u`, plus the literal
// `/[^\\]\\Z/` guard. These are the divergence shapes observed in review:
// JS-invalid patterns Rust accepts and JS-valid Annex B patterns Rust rejects.
for (const [name, value] of [
  ['anchored', '^a+$'],
  ['unterminated-class', '['],
  ['lookahead', '(?=a)'],
  ['identity-hyphen', '\\-'],
  ['lone-close-bracket', 'a]'],
  ['identity-angle', '\\<a'],
  ['inline-flags', '(?i:a)'],
  ['double-quantifier', 'a**'],
  ['double-range-quantifier', 'x{2}{3}'],
  ['open-lower-quantifier', 'a{,5}'],
  ['control-j', '\\cJ'],
  ['nul', '\\0'],
  ['bare-k', '\\k'],
  ['anchor-z', '\\Z'],
  ['anchor-z-after', 'a\\Z'],
  ['literal-z-after', 'a\\\\Z'],
  ['property-without-u', '\\p{L}'],
  ['named-group', '(?<x>a)'],
  ['named-backreference', '(?<x>a)\\k<x>'],
  ['unknown-named-backreference', '(?<x>a)\\k<y>'],
  ['reversed-range', 'a{2,1}'],
  ['literal-open-brace', 'a{2x}'],
  ['spaced-brace', 'a{2, 3}'],
  ['empty-group', '()'],
  ['empty-class', '[]'],
  ['negated-empty-class', '[^]'],
  ['empty-class-close', '[]]'],
  ['class-shorthand-range-right', '[a-\\w]'],
  ['class-shorthand-range-left', '[\\w-a]'],
  ['identity-eight', '\\8'],
  ['identity-two', '\\2'],
  ['backreference-two', '(a)\\2'],
  ['short-hex', '\\x'],
  ['short-hex-one', '\\x1'],
  ['short-unicode', '\\u'],
  ['short-unicode-one', '\\u1'],
  ['brace-unicode-without-u', '\\u{41}'],
  ['control-digit', '\\c1'],
  ['control-class', '\\c['],
  ['trailing-backslash', '\\'],
  ['quantified-lookbehind', '(?<=a)*'],
  ['lookbehind-literal-open-lower', '(?<=a){,3}'],
  ['lookbehind-literal-open-brace', '(?<=a){2x}'],
  ['lookbehind', '(?<=a)'],
  ['quantified-lookahead', '(?=a)*'],
  ['rust-named-group', '(?P<x>a)'],
  ['digit-group-name', '(?<1a>x)'],
  ['astral-class-range', '[😀-😁]'],
  ['double-dash-order', '[a--b]'],
  ['double-dash-start', '[--a]'],
  ['standalone-named-reference', '\\k<x>'],
  ['empty-named-reference', '\\k<>'],
  ['empty-named-reference-with-group', '(?<x>a)\\k<>'],
  ['identity-q', '\\q'],
  ['class-identity-q', '[\\q]'],
  ['literal-brace-after-star', 'a*{x}'],
  ['quantifier-after-star', 'a*{2}'],
  ['lazy-star', 'a*?'],
  ['lazy-range', 'a{2,3}?'],
]) add(`format-regex/${name}`, {type: 'string', format: 'regex'}, value);

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
  ['lone-surrogate-escape', {pattern: '\\uD83D'}, '\uD83D'],
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

// r5: group nesting must not consume the Rust worker stack. V8's parser
// limit is 32767 captures, not group depth: even 100000 noncapturing groups
// construct successfully. Matching deep captures/lookaheads may throw later;
// record that separately from a schema compile failure.
for (const [kind, open] of [
  ['capture', '('],
  ['noncapture', '(?:'],
  ['lookahead', '(?='],
]) {
  for (const depth of [10000, 100000]) {
    const pattern = open.repeat(depth) + ')'.repeat(depth);
    const unsupported = kind !== 'capture' || depth <= 32767;
    cases.push({
      id: `deep-pattern/${kind}-${depth}`,
      schema: {pattern}, data: '',
      ...(unsupported ? {portUnsupported: true} : {}),
    });
    cases.push({
      id: `deep-pattern-properties/${kind}-${depth}`,
      schema: {patternProperties: {[pattern]: {type: 'number'}}}, data: {x: 1},
      ...(unsupported ? {portUnsupported: true} : {}),
    });
    add(`deep-format-regex/${kind}-${depth}`, {type: 'string', format: 'regex'}, pattern);
  }
}
for (const depth of [32767, 32768]) {
  const pattern = '('.repeat(depth) + ')'.repeat(depth);
  cases.push({
    id: `deep-pattern/capture-${depth}`, schema: {pattern}, data: '',
    ...(depth === 32767 ? {portUnsupported: true} : {}),
  });
  add(`deep-format-regex/capture-${depth}`, {type: 'string', format: 'regex'}, pattern);
  add(`deep-format-regex/identity-k-${depth}`, {type: 'string', format: 'regex'}, `\\k<${pattern}>`);
}
for (const [name, pattern] of [
  ['identity-unclosed-group', '\\k<(>'],
  ['identity-class', '\\k<[]>'],
  ['forward-reference', '\\k<x>(?<x>a)'],
  ['missing-forward-reference', '\\k<y>(?<x>a)'],
  ['class-before-named-group', '[\\k](?<x>a)'],
  ['class-after-named-group', '(?<x>a)[\\k]'],
  ['invalid-reference-name', '\\k<(?<x>a)>'],
]) add(`format-regex-named/${name}`, {type: 'string', format: 'regex'}, pattern);

for (const [name, pattern] of [
  ['dollar', '^(?<$>x)$'],
  ['escaped', '^(?<\\u0061>x)$'],
  ['unicode', '^(?<中>x)$'],
]) {
  for (const data of ['x', 'y']) {
    add(`named-group/${name}-${data}`, {pattern}, data);
  }
}

// Store iterative recipes, avoiding JSON stringify/parser recursion limits.
// Each sample is a cold, default-stack *real Client.callTool* observation.
// The port's budget is 1024 weighted active schema units, not data depth.
let recursiveDataTemplate = fs.readFileSync(
  fileURLToPath(new URL('./recursive-data.mjs', import.meta.url)), 'utf8',
);
const sdkFiles = {
  client: 'client/index.js', server: 'server/index.js', memory: 'inMemory.js', types: 'types.js',
};
const recursiveSdkPins = {
  "node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js": "a5e17addde5d50e0b2a4ac5961d0f59fd044ff47b01b69e715c3b7ab23075bbd",
  "node_modules/@modelcontextprotocol/sdk/dist/esm/server/index.js": "8dbee2ac3001c9685ad49a2d6b2c59973ca0b223579ec353f9c2e199484591b0",
  "node_modules/@modelcontextprotocol/sdk/dist/esm/inMemory.js": "8eb57dc4b4c0993869273a5e01a35c71e8d84c3d5050e9871a8cf141360f883d",
  "node_modules/@modelcontextprotocol/sdk/dist/esm/types.js": "962836b0f8dad85bcd398ad3ddb5ba81a7c7530c706955aa846dd8dfc02dd6a9"
};
const recursiveSdkSha256 = {};
for (const [name, file] of Object.entries(sdkFiles)) {
  const marker = `'pdrs:baseline-${name}'`;
  assert.equal(recursiveDataTemplate.split(marker).length, 2);
  const relative = `node_modules/@modelcontextprotocol/sdk/dist/esm/${file}`;
  recursiveSdkSha256[relative] = hash(relative);
  assert.equal(recursiveSdkSha256[relative], recursiveSdkPins[relative], `recursive SDK source drift: ${relative}`);
  recursiveDataTemplate = recursiveDataTemplate.replace(marker, JSON.stringify(pathToFileURL(path.join(baseline, relative)).href));
}
const recursiveDataScratch = fs.mkdtempSync(path.join(os.tmpdir(), 'pdrs-recursive-data-'));
const recursiveDataProbe = path.join(recursiveDataScratch, 'probe.mjs');
fs.writeFileSync(recursiveDataProbe, recursiveDataTemplate);
const recursiveDataEnv = {...process.env};
delete recursiveDataEnv.NODE_OPTIONS;
delete recursiveDataEnv.NODE_COMPILE_CACHE;
const overflow = {code: -32602, message: 'MCP error -32602: Failed to validate structured content: Maximum call stack size exceeded'};
const observe = (shape, depth) => JSON.parse(execFileSync(process.execPath, [
  recursiveDataProbe, shape, String(depth),
], {env: recursiveDataEnv, encoding: 'utf8'}));
try {
  for (const [shape, lastSupported] of [
    ['properties', 145], ['patternProperties', 145], ['additionalProperties', 170],
    ['items', 168], ['anyOf', 67],
  ]) {
    for (const depth of [0, lastSupported, lastSupported + 1]) {
      const observed = observe(shape, depth);
      if (depth <= lastSupported) assert.ok(!observed.sdkError, `SDK must pass below port budget: ${shape}/${depth}`);
      else if (observed.sdkError) assert.deepEqual(observed.sdkError, overflow);
      cases.push({id: `recursive-data/${shape}-${depth}`, ...observed,
        ...(depth > lastSupported ? {portDepthLimit: true, ...(observed.sdkError ? {} : {knownDifference: 'conservative-stack-budget'})} : {}),
      });
    }
    // Find an over-SDK-boundary sample dynamically. No V8-specific breakpoint
    // is hard-coded as an assertion; the bounded search fails loudly if absent.
    let depth = 1024;
    let observed;
    for (; depth <= 32768; depth *= 2) {
      observed = observe(shape, depth);
      if (observed.sdkError) break;
    }
    assert.ok(depth <= 32768, `no SDK overflow sample found: ${shape}`);
    assert.deepEqual(observed.sdkError, overflow);
    cases.push({id: `recursive-data/${shape}-sdk-overflow`, ...observed, portDepthLimit: true});
  }
  for (const [kind, lastSupported] of [['const', 510], ['enum', 509], ['uniqueItems', 1016]]) {
    const regularError = {code: -32602, message: "MCP error -32602: Structured content does not match the tool's output schema: data/a must NOT have duplicate items (items ## 0 and 1 are identical)"};
    for (const depth of [lastSupported, lastSupported + 1]) {
      const observed = observe(kind, depth);
      assert.deepEqual(observed.sdkError, kind === 'uniqueItems' ? regularError : undefined, `SDK below comparison safety margin: ${kind}/${depth}`);
      cases.push({id: `recursive-comparison/${kind}-${depth}`, ...observed,
        ...(depth > lastSupported ? {portDepthLimit: true, knownDifference: 'conservative-stack-budget'} : {}),
      });
    }
    let depth = 1024;
    let observed;
    for (; depth <= 32768; depth *= 2) {
      observed = observe(kind, depth);
      if (observed.sdkError?.message === overflow.message) break;
    }
    assert.ok(depth <= 32768, `no SDK comparison overflow sample found: ${kind}`);
    assert.deepEqual(observed.sdkError, overflow);
    cases.push({id: `recursive-comparison/${kind}-sdk-overflow`, ...observed, portDepthLimit: true});
  }
  for (const depth of [0, 340]) {
    const observed = observe('wideAnyOf', depth);
    if (observed.sdkError) assert.deepEqual(observed.sdkError, overflow);
    cases.push({id: `recursive-data/wideAnyOf-${depth}`, ...observed, portDepthLimit: true,
      ...(observed.sdkError ? {} : {knownDifference: 'conservative-stack-budget'}),
    });
  }
} finally {
  fs.rmSync(recursiveDataScratch, {recursive: true, force: true});
}

Object.assign(cases.find(({id}) => id === 'ref/recursive-499'), {portDepthLimit: true, knownDifference: 'conservative-stack-budget'});

const oracle = {
  sourceCommit, sourceSha256, regexCompilation, recursiveSdkSha256,
  sdk: sdkPackage.version,
  ajv: ajvPackage.version,
  ajvFormats: formatsPackage.version,
  providerSha256: hash('node_modules/@modelcontextprotocol/sdk/dist/esm/validation/ajv-provider.js'),
  ajvSha256: hash('node_modules/ajv/dist/ajv.js'),
  formatsSha256: hash('node_modules/ajv-formats/dist/formats.js'),
  recursiveDataRuntime: {
    node: process.version, v8: process.versions.v8,
    platform: process.platform, arch: process.arch,
    coldStart: true, defaultStack: true, clientCallTool: true, validationBudget: 1024,
  },
  cases: cases.map((entry) => {
    if (entry.recursiveData || entry.comparisonData) return entry;
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
      if (compiled && entry.portUnsupported) {
        return {...entry, validationThrow: {name: error.name, message: error.message}};
      }
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
