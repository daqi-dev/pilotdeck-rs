// Node 22 + fixed baseline SDK; writes only to the supplied output directory.
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
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
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


// Inputs only: every expected compile/match result below comes from the real
// SDK provider, not a hand-written interpretation of ECMAScript.
const cases = [];
const add = (id, pattern, positive, negative) => {
  for (const [kind, data] of [['positive', positive], ['negative', negative]]) {
    cases.push({id: `${id}/value/${kind}`, schema: {type: 'string', pattern}, data});
    // A matching key must validate its child; nonmatching keys must fail
    // additionalProperties. This exercises both patternProperties call sites.
    cases.push({id: `${id}/key/${kind}`, schema: {
      type: 'object', patternProperties: {[pattern]: {type: 'number'}}, additionalProperties: false,
    }, data: {[data]: 7}});
  }
};
add('lookbehind/positive', '(?<=prefix:)value$', 'prefix:value', 'other:value');
add('lookbehind/negative', '(?<!prefix:)value$', 'other:value', 'prefix:value');
add('lookbehind/reverse-captures', '(?<=([ab]+)([bc]+))$', 'abc', 'xyz');
add('lookahead/positive', '^(?=a)a$', 'a', 'b');
add('lookahead/negative', '^(?!a)b$', 'b', 'a');
add('backreference/number', '^(a|b)\\1$', 'aa', 'ab');
add('backreference/name', '^(?<word>[a-z]+):\\k<word>$', 'abc:abc', 'abc:abd');
add('backreference/forward', '^\\1(a)$', 'a', 'aa');
add('backreference/unmatched', '^(?:(a)|b)\\1$', 'b', 'ba');
add('property/currency', '^\\p{Sc}+$', '$€', 'ab');
add('property/script-extensions', '^\\p{Script_Extensions=Hira}+$', 'ーあ', 'A');
add('property/complement', '^\\P{ASCII}+$', 'é😀', 'a');
add('property/surrogate', '^\\p{Surrogate}$', '\ud800', '�');
add('utf16/lone-high', '^\\uD800$', '\ud800', '�');
add('utf16/lone-low', '^\\uDFFF$', '\udfff', '\ud800');
add('utf16/dot-lone', '^.$', '\ud800', 'ab');
add('utf16/astral-unit', '^.$', '😀', '😀a');
add('utf16/lone-is-not-pair', '^\\uD83D$', '\ud83d', '😀');
add('utf16/surrogate-backreference', '^(.)\\1$', '\ud800\ud800', '\ud800\udfff');
add('utf16/raw-lone-pattern', '^\ud800$', '\ud800', '�');
// These class operators are literals under Ajv's u flag. Accidentally enabling
// v changes their meaning (or makes them invalid).
add('u-not-v/intersection', '^[a&&b]$', '&', 'c');
add('u-not-v/double-tilde', '^[a~~b]$', '~', 'c');
add('u-not-v/empty-class', '^[^]$', '\ud800', '');
add('u-not-v/nested-open', '^[[]$', '[', 'a');
for (const [id, pattern] of [
  ['class-string', '^\\p{RGI_Emoji}$'],
  ['class-string-q', '^[\\q{ab|cd}]$'],
  ['nested-set', '^[[a]--[b]]$'],
  ['inline-v-flag', '(?v:a)'],
  ['u-identity-escape', '^\\a$'],
  ['u-identity-in-class', '^[\\q]$'],
]) {
  for (const target of ['value', 'key']) {
    cases.push({id: `syntax/${id}/${target}`, schema: target === 'value'
      ? {type: 'string', pattern}
      : {type: 'object', patternProperties: {[pattern]: {type: 'number'}}}, data: target === 'value' ? '' : {a: 1}});
  }
}
const observed = cases.map(entry => {
  let validate;
  try { validate = new AjvJsonSchemaValidator().getValidator(entry.schema); }
  catch (error) { return {...entry, compileError: {name: error.name, message: error.message}}; }
  try { const result = validate(entry.data); return {...entry, valid: result.valid, ...(result.errorMessage === undefined ? {} : {errorText: result.errorMessage})}; }
  catch (error) { return {...entry, validationThrow: {name: error.name, message: error.message}}; }
});
const oracle = {
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8,
  sdk: sdkPackage.version, ajv: ajvPackage.version, ajvFormats: formatsPackage.version,
  providerSha256: hash('node_modules/@modelcontextprotocol/sdk/dist/esm/validation/ajv-provider.js'),
  ajvSha256: hash('node_modules/ajv/dist/ajv.js'),
  formatsSha256: hash('node_modules/ajv-formats/dist/formats.js'),
  regexCompilation, cases: observed,
};
fs.mkdirSync(outDir, {recursive: true});
const target = path.join(outDir, 'regress-oracle.json');
fs.writeFileSync(target, `${JSON.stringify(oracle, null, 2)}\n`);
console.log(`wrote ${target}: ${observed.length} cases`);
