// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Inputs are built as real JavaScript snapshots (including NaN/undefined/-0) and
// every recorded result comes from the fixed TypeScript baseline itself.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', 'baseline must be the fixed PilotDeck commit');
const source = 'src/pilot/config/classifyChanges.ts';
const { diffConfigSnapshots, classifyConfigChanges, isRunPolicyOnlyChange } =
  await import(pathToFileURL(join(baseline, source)).href);
const { mergeConfigSources } = await import(pathToFileURL(join(baseline, 'src/pilot/config/merge.ts')).href);
const { parseModulesConfig } = await import(pathToFileURL(join(baseline, 'src/pilot/config/parseModulesConfig.ts')).href);

// JSON has no undefined/NaN/Infinity/-0, and no explicit [[Prototype]]. Encode
// those special values as {"$":tag}; an object whose prototype is not
// Object.prototype becomes {"$":"prototype","prototype":...,"value":...}. Every
// other value is plain JSON. The generator rejects a real object using the
// reserved "$" key, so decoding is unambiguous.
function encode(value: unknown): unknown {
  if (value === undefined) return { $: 'undefined' };
  if (typeof value === 'number') {
    if (Number.isNaN(value)) return { $: 'NaN' };
    if (value === Infinity) return { $: 'Infinity' };
    if (value === -Infinity) return { $: '-Infinity' };
    if (Object.is(value, -0)) return { $: '-0' };
    return value;
  }
  if (Array.isArray(value)) return value.map(encode);
  if (value !== null && typeof value === 'object') {
    const encoded: Record<string, unknown> = Object.create(null);
    for (const key of Object.keys(value)) {
      assert.notEqual(key, '$', 'fixture object uses the reserved "$" key');
      encoded[key] = encode((value as Record<string, unknown>)[key]);
    }
    if (Object.getPrototypeOf(value) !== Object.prototype) {
      return { $: 'prototype', prototype: encode(Object.getPrototypeOf(value)), value: encoded };
    }
    return encoded;
  }
  return value;
}

const snapshot = (config: unknown, extra: Record<string, unknown> = {}) => ({ schemaVersion: 1, version: 1, ...extra, config });
const pairs: any[] = [];
function pair(name: string, previous: unknown, next: unknown) {
  const changedPaths = diffConfigSnapshots(previous as any, next as any);
  const changeClasses = classifyConfigChanges(changedPaths);
  const runPolicyOnly = isRunPolicyOnlyChange(changedPaths);
  for (const path of changedPaths) assert.equal(typeof path, 'string');
  pairs.push({ name, previous: encode(previous), next: encode(next), changedPaths, changeClasses, runPolicyOnly });
}
const cases: any[] = [];
function classify(name: string, paths: string[]) {
  const classes = classifyConfigChanges(paths);
  const runPolicyOnly = isRunPolicyOnlyChange(paths);
  cases.push({ name, paths, classes, runPolicyOnly });
}

// The real product path: mergeConfigSources -> parseModulesConfig keeps the
// tool `inputSchema` object verbatim, including a JSON.parse-created own
// "__proto__" key.
function modulesSnapshot(inputSchema: unknown) {
  const raw = {
    modules: {
      tools: {
        enabled: true,
        implementationId: 'acme.tools',
        contract: 'pilotdeck.tools/v1',
        transport: 'module-http-v2',
        endpoint: 'https://example.com',
        catalog: [{ name: 't', description: 'd', inputSchema }],
      },
    },
  };
  const merged = mergeConfigSources(raw);
  const diagnostics: unknown[] = [];
  const modules = parseModulesConfig(merged.modules, '/pilot-home', diagnostics as any, {});
  assert.equal(diagnostics.length, 0, 'inputSchema fixture must parse without diagnostics');
  return snapshot({ modules });
}

// ---------------------------------------------------------------------------
// Snapshot pairs: every classifyPath/classifyRouterPath branch, router
// sub-paths, container add/remove/change and JS value boundaries.
// ---------------------------------------------------------------------------
pair('identical-config', snapshot({ a: 1, nested: { b: [1, 2] } }), snapshot({ a: 1, nested: { b: [1, 2] } }));
pair('non-config-only', snapshot({ a: 1 }, { version: 1, diagnostics: [] }), snapshot({ a: 1 }, { version: 2, diagnostics: [{ code: 'X' }] }));
pair('root-scalar', snapshot(1), snapshot(2));
pair('root-null-vs-undefined', snapshot(null), snapshot(undefined));
pair('root-absent-both', snapshot(undefined), snapshot(undefined));
pair('agent-leaf', snapshot({ agent: { model: 'a' } }), snapshot({ agent: { model: 'b' } }));
pair('model-leaf', snapshot({ model: { provider: 'a' } }), snapshot({ model: { provider: 'b' } }));
pair('extension-include-hook-exact', snapshot({ extension: { includeHookEvents: false } }), snapshot({ extension: { includeHookEvents: true } }));
pair('extension-include-hook-child', snapshot({ extension: { includeHookEvents: { a: 1 } } }), snapshot({ extension: { includeHookEvents: { a: 2 } } }));
pair('extension-other', snapshot({ extension: { foo: 1 } }), snapshot({ extension: { foo: 2 } }));
pair('router-scenarios', snapshot({ router: { scenarios: { x: 1 } } }), snapshot({ router: { scenarios: { x: 2 } } }));
pair('router-fallback', snapshot({ router: { fallback: { a: 1 } } }), snapshot({ router: { fallback: { a: 2 } } }));
pair('router-token-tiers', snapshot({ router: { tokenSaver: { tiers: { a: 1 } } } }), snapshot({ router: { tokenSaver: { tiers: { a: 2 } } } }));
pair('router-zero-attempts', snapshot({ router: { zeroUsageRetry: { maxAttempts: 1 } } }), snapshot({ router: { zeroUsageRetry: { maxAttempts: 2 } } }));
pair('router-zero-enabled', snapshot({ router: { zeroUsageRetry: { enabled: false } } }), snapshot({ router: { zeroUsageRetry: { enabled: true } } }));
pair('router-token-judge', snapshot({ router: { tokenSaver: { judge: { model: 'a' } } } }), snapshot({ router: { tokenSaver: { judge: { model: 'b' } } } }));
pair('router-auto-orchestrate', snapshot({ router: { autoOrchestrate: { skillExtensionId: 'a' } } }), snapshot({ router: { autoOrchestrate: { skillExtensionId: 'b' } } }));
pair('router-stats', snapshot({ router: { stats: { enabled: false } } }), snapshot({ router: { stats: { enabled: true } } }));
pair('router-custom', snapshot({ router: { customRouter: { extensionId: 'a' } } }), snapshot({ router: { customRouter: { extensionId: 'b' } } }));
pair('router-default', snapshot({ router: { enabled: false } }), snapshot({ router: { enabled: true } }));
pair('router-transient-enabled', snapshot({ router: { transientRetry: { enabled: true } } }), snapshot({ router: { transientRetry: { enabled: false } } }));
pair('router-transient-max-attempts', snapshot({ router: { transientRetry: { maxAttempts: 2 } } }), snapshot({ router: { transientRetry: { maxAttempts: 3 } } }));
pair('router-transient-delays', snapshot({ router: { transientRetry: { baseDelayMs: 500, maxDelayMs: 8000 } } }), snapshot({ router: { transientRetry: { baseDelayMs: 750, maxDelayMs: 9000 } } }));
pair('router-transient-container-replaced', snapshot({ router: { transientRetry: { enabled: true } } }), snapshot({ router: { transientRetry: 1 } }));
pair('always-on', snapshot({ alwaysOn: { x: 1 } }), snapshot({ alwaysOn: { x: 2 } }));
pair('cron', snapshot({ cron: { x: 1 } }), snapshot({ cron: { x: 2 } }));
pair('tools', snapshot({ tools: { x: 1 } }), snapshot({ tools: { x: 2 } }));
pair('modules-exact', snapshot({ modules: { a: 1 } }), snapshot({ modules: null }));
pair('modules-child', snapshot({ modules: { a: 1 } }), snapshot({ modules: { a: 2 } }));
pair('proxy-exact', snapshot({ proxy: 1 }), snapshot({ proxy: 2 }));
pair('proxy-child', snapshot({ proxy: { a: 1 } }), snapshot({ proxy: { a: 2 } }));
pair('run-policy-exact', snapshot({ runPolicy: 1 }), snapshot({ runPolicy: 2 }));
pair('run-policy-child', snapshot({ runPolicy: { a: 1 } }), snapshot({ runPolicy: { a: 2 } }));
pair('run-policy-only-multi', snapshot({ runPolicy: { a: 1, b: 1 } }), snapshot({ runPolicy: { a: 2, b: 2 } }));
pair('run-policy-plus-other', snapshot({ runPolicy: { a: 1 }, agent: { model: 'x' } }), snapshot({ runPolicy: { a: 2 }, agent: { model: 'y' } }));
pair('default-unknown', snapshot({ webui: { x: 1 } }), snapshot({ webui: { x: 2 } }));

pair('array-same-length', snapshot({ arr: [1, 2, 3] }), snapshot({ arr: [1, 9, 3] }));
pair('array-length', snapshot({ arr: [1] }), snapshot({ arr: [1, 2] }));
pair('array-vs-object', snapshot({ arr: [1] }), snapshot({ arr: { 0: 1 } }));
pair('array-object-leaf', snapshot({ arr: [{ x: 1, y: 2 }] }), snapshot({ arr: [{ x: 1, y: 3 }] }));
pair('object-add-leaf', snapshot({ a: { x: 1 } }), snapshot({ a: { x: 1, y: 2 } }));
pair('object-remove-leaf', snapshot({ a: { x: 1, y: 2 } }), snapshot({ a: { x: 1 } }));
pair('object-add-object', snapshot({ a: { x: 1 } }), snapshot({ a: { x: 1, y: { z: 2 } } }));
pair('nested-array-leaf', snapshot({ a: { b: [{ c: 1 }] }, d: 1 }), snapshot({ a: { b: [{ c: 2 }] }, d: 1 }));
pair('root-array-same-length', snapshot([1, 2]), snapshot([1, 3]));
pair('root-array-length', snapshot([1]), snapshot([1, 2]));
pair('root-object-vs-array', snapshot({ a: 1 }), snapshot([1]));

pair('nan-same', snapshot({ n: NaN }), snapshot({ n: NaN }));
pair('nan-vs-zero', snapshot({ n: NaN }), snapshot({ n: 0 }));
pair('nan-vs-null', snapshot({ n: NaN }), snapshot({ n: null }));
pair('null-same', snapshot({ n: null }), snapshot({ n: null }));
pair('null-vs-undefined', snapshot({ n: null }), snapshot({ n: undefined }));
pair('undefined-same', snapshot({ n: undefined }), snapshot({ n: undefined }));
pair('undefined-removed', snapshot({ n: undefined, a: 1 }), snapshot({ a: 1 }));
pair('undefined-added', snapshot({ a: 1 }), snapshot({ a: 1, n: undefined }));
pair('negative-zero', snapshot({ n: -0 }), snapshot({ n: 0 }));
pair('infinity-same', snapshot({ n: Infinity }), snapshot({ n: Infinity }));
pair('infinity-vs-null', snapshot({ n: Infinity }), snapshot({ n: null }));
pair('array-specials-same', snapshot({ arr: [NaN, null, undefined] }), snapshot({ arr: [NaN, null, undefined] }));
pair('bool-vs-number', snapshot({ v: true }), snapshot({ v: 1 }));
pair('string-change', snapshot({ s: 'a' }), snapshot({ s: 'b' }));

pair('key-sort-numeric', snapshot({ b: 1, 10: 1, 2: 1, a: 1 }), snapshot({ b: 2, 10: 2, 2: 2, a: 2 }));
pair('key-sort-utf16', snapshot({ '\uE000': 1, '\u{10000}': 1 }), snapshot({ '\uE000': 2, '\u{10000}': 2 }));
pair('lone-surrogate-key', snapshot({ '\uD800': 1 }), snapshot({ '\uD800': 2 }));
pair('mixed-classes', snapshot({
  agent: { model: 'a' },
  extension: { includeHookEvents: false },
  modules: { a: 1 },
  proxy: { a: 1 },
  z: 1,
}), snapshot({
  agent: { model: 'b' },
  extension: { includeHookEvents: true },
  modules: { a: 2 },
  proxy: { a: 2 },
  z: 2,
}));

// ---------------------------------------------------------------------------
// JavaScript prototype lookup: implicit Object.prototype builtins, literal
// `__proto__` own keys, explicit prototypes (Object.create) and -Infinity.
// ---------------------------------------------------------------------------
const ownProto = (json: string) => JSON.parse(json);
function ownUndefined(names: string[]) {
  const value: Record<string, unknown> = {};
  for (const name of names) {
    Object.defineProperty(value, name, { value: undefined, enumerable: true, configurable: true, writable: true });
  }
  return value;
}
const OBJECT_PROTOTYPE_MEMBERS = [
  'constructor', 'hasOwnProperty', 'isPrototypeOf', 'propertyIsEnumerable',
  'toLocaleString', 'toString', 'valueOf', '__proto__',
  '__defineGetter__', '__defineSetter__', '__lookupGetter__', '__lookupSetter__',
];
const ARRAY_PROTOTYPE_METHODS = Object.getOwnPropertyNames(Array.prototype)
  .filter((name) => name !== 'length' && name !== '__proto__');
pair('proto-own-empty-vs-missing', snapshot(ownProto('{"__proto__":{}}')), snapshot({}));
pair('proto-own-empty-vs-missing-reversed', snapshot({}), snapshot(ownProto('{"__proto__":{}}')));
pair('proto-own-x-vs-missing', snapshot(ownProto('{"__proto__":{"x":1}}')), snapshot({}));
pair('proto-own-x-vs-missing-reversed', snapshot({}), snapshot(ownProto('{"__proto__":{"x":1}}')));
pair('proto-own-null-vs-missing', snapshot(ownProto('{"__proto__":{"__proto__":null}}')), snapshot({}));
pair('proto-own-toString-null', snapshot(ownProto('{"__proto__":{"toString":null}}')), snapshot({}));
pair('toString-undefined-vs-empty', snapshot({ toString: undefined }), snapshot({}));
pair('toString-undefined-vs-empty-reversed', snapshot({}), snapshot({ toString: undefined }));
pair('constructor-undefined-vs-empty', snapshot({ constructor: undefined }), snapshot({}));
pair('hasOwnProperty-undefined-vs-empty', snapshot({ hasOwnProperty: undefined }), snapshot({}));
pair('explicit-array-prototype-index', snapshot(Object.create([7])), snapshot({ 0: 7 }));
pair('explicit-array-prototype-length', snapshot(Object.create([7])), snapshot({ length: 1 }));
pair('explicit-array-prototype-method', snapshot(Object.create([7])), snapshot({ push: undefined }));
pair('explicit-object-prototype-child', snapshot(Object.create({ x: 1 })), snapshot({ x: 2 }));
pair('explicit-object-prototype-vs-own-proto', snapshot(Object.create({ x: 1 })), snapshot(ownProto('{"__proto__":{"x":1}}')));
pair('explicit-proto-null-toString', snapshot(Object.create(null)), snapshot({ toString: undefined }));
pair('explicit-proto-nested-chain', snapshot(Object.create(Object.create({ x: 1 }))), snapshot({ x: 2 }));
pair('own-key-shadows-explicit-proto', snapshot(Object.assign(Object.create({ x: 1 }), { x: 2 })), snapshot({ x: 1 }));
pair('explicit-array-proto-proto', snapshot(Object.create([7])), snapshot(ownProto('{"__proto__":[7]}')));
pair('negative-infinity-vs-zero', snapshot({ n: -Infinity }), snapshot({ n: 0 }));
pair('negative-infinity-same', snapshot({ n: -Infinity }), snapshot({ n: -Infinity }));
pair('negative-infinity-vs-null', snapshot({ n: -Infinity }), snapshot({ n: null }));
pair('object-prototype-members-all-undefined', snapshot(ownUndefined(OBJECT_PROTOTYPE_MEMBERS)), snapshot({}));
pair('array-prototype-methods-all-undefined', snapshot(Object.create([7])), snapshot(ownUndefined(ARRAY_PROTOTYPE_METHODS)));
pair('inputSchema-own-proto-empty', modulesSnapshot(ownProto('{"__proto__":{}}')), modulesSnapshot({}));
pair('inputSchema-own-proto-x', modulesSnapshot(ownProto('{"__proto__":{"x":1}}')), modulesSnapshot({}));

// ---------------------------------------------------------------------------
// Direct classify inputs: exact/prefix edges, first-seen order and dedupe.
// ---------------------------------------------------------------------------
classify('empty', []);
classify('agent-exact-default', ['agent']);
classify('agent-dot', ['agent.']);
classify('model-exact-default', ['model']);
classify('model-dot', ['model.']);
classify('extension-exact-default', ['extension']);
classify('extension-include-hook-child', ['extension.includeHookEvents.x']);
classify('router-exact-default', ['router']);
classify('router-judge-prefix', ['router.tokenSaver.judge']);
classify('router-judge-longer-prefix', ['router.tokenSaver.judgement']);
classify('router-zero-suffix-not-exact', ['router.zeroUsageRetry.maxAttempts.x']);
classify('router-transient-exact', ['router.transientRetry']);
classify('router-transient-fields', ['router.transientRetry.enabled', 'router.transientRetry.maxAttempts', 'router.transientRetry.baseDelayMs', 'router.transientRetry.maxDelayMs']);
classify('router-transient-suffix-not-exact', ['router.transientRetry.maxAttempts.x', 'router.transientRetry.enabled.x', 'router.transientRetry.unknown']);
classify('modules-exact', ['modules']);
classify('modules-dot', ['modules.']);
classify('proxy-dot', ['proxy.']);
classify('run-policy-dot', ['runPolicy.']);
classify('run-policy-prefix-only', ['runPolicyX']);
classify('class-order-dedupe', ['modules.a', 'agent.x', 'modules.b', 'proxy']);
classify('run-policy-list', ['runPolicy', 'runPolicy.a']);
classify('all-next-runtime', ['alwaysOn.x', 'cron.y', 'tools.z', 'webui.q']);
classify('default-empty-segment', ['']);

// Guardrails: the fixture set must be non-vacuous and cover every observable class.
assert.ok(pairs.length >= 25, `need >= 25 snapshot pairs, got ${pairs.length}`);
assert.equal(new Set(pairs.map(p => p.name)).size, pairs.length, 'duplicate pair name');
assert.equal(new Set(cases.map(c => c.name)).size, cases.length, 'duplicate classify case name');
const seen = new Set([...pairs.flatMap(p => p.changeClasses), ...cases.flatMap(c => c.classes)]);
assert.deepEqual([...seen].sort(), ['next-request', 'next-runtime', 'restart-required', 'runtime-live'], 'class coverage');
const byName = new Map(pairs.map(p => [p.name, p]));
assert.deepEqual(byName.get('identical-config').changedPaths, []);
assert.deepEqual(byName.get('non-config-only').changedPaths, []);
assert.deepEqual(byName.get('root-scalar').changedPaths, ['<root>']);
assert.deepEqual(byName.get('array-length').changedPaths, ['arr']);
assert.deepEqual(byName.get('array-same-length').changedPaths, ['arr[1]']);
assert.deepEqual(byName.get('object-add-object').changedPaths, ['a.y']);
assert.deepEqual(byName.get('nan-same').changedPaths, []);
assert.deepEqual(byName.get('nan-vs-zero').changedPaths, ['n']);
assert.deepEqual(byName.get('undefined-removed').changedPaths, []);
assert.deepEqual(byName.get('undefined-added').changedPaths, []);
assert.deepEqual(byName.get('negative-zero').changedPaths, ['n']);
assert.deepEqual(byName.get('key-sort-numeric').changedPaths, ['10', '2', 'a', 'b']);
assert.deepEqual(byName.get('key-sort-utf16').changedPaths, ['\u{10000}', '\uE000']);
assert.equal(byName.get('run-policy-only-multi').runPolicyOnly, true);
assert.equal(byName.get('run-policy-plus-other').runPolicyOnly, false);
assert.equal(byName.get('run-policy-exact').runPolicyOnly, true);
assert.equal(byName.get('mixed-classes').runPolicyOnly, false);
assert.deepEqual(byName.get('mixed-classes').changeClasses, ['next-request', 'runtime-live', 'restart-required', 'next-runtime']);
assert.equal(cases.find(c => c.name === 'run-policy-prefix-only').runPolicyOnly, false);
assert.equal(cases.find(c => c.name === 'empty').runPolicyOnly, false);
assert.deepEqual(byName.get('proto-own-empty-vs-missing').changedPaths, []);
assert.deepEqual(byName.get('proto-own-empty-vs-missing-reversed').changedPaths, []);
assert.deepEqual(byName.get('proto-own-x-vs-missing').changedPaths, ['__proto__.x']);
assert.deepEqual(byName.get('proto-own-x-vs-missing-reversed').changedPaths, ['__proto__.x']);
assert.deepEqual(byName.get('proto-own-null-vs-missing').changedPaths, []);
assert.deepEqual(byName.get('proto-own-toString-null').changedPaths, ['__proto__.toString']);
assert.deepEqual(byName.get('toString-undefined-vs-empty').changedPaths, ['toString']);
assert.deepEqual(byName.get('toString-undefined-vs-empty-reversed').changedPaths, ['toString']);
assert.deepEqual(byName.get('constructor-undefined-vs-empty').changedPaths, ['constructor']);
assert.deepEqual(byName.get('hasOwnProperty-undefined-vs-empty').changedPaths, ['hasOwnProperty']);
assert.deepEqual(byName.get('explicit-array-prototype-index').changedPaths, []);
assert.deepEqual(byName.get('explicit-array-prototype-length').changedPaths, []);
assert.deepEqual(byName.get('explicit-array-prototype-method').changedPaths, ['push']);
assert.deepEqual(byName.get('explicit-object-prototype-child').changedPaths, ['x']);
assert.deepEqual(byName.get('explicit-object-prototype-vs-own-proto').changedPaths, []);
assert.deepEqual(byName.get('explicit-proto-null-toString').changedPaths, []);
assert.deepEqual(byName.get('explicit-proto-nested-chain').changedPaths, ['x']);
assert.deepEqual(byName.get('own-key-shadows-explicit-proto').changedPaths, ['x']);
assert.deepEqual(byName.get('explicit-array-proto-proto').changedPaths, []);
assert.deepEqual(byName.get('object-prototype-members-all-undefined').changedPaths, [...OBJECT_PROTOTYPE_MEMBERS].sort());
assert.deepEqual(byName.get('array-prototype-methods-all-undefined').changedPaths, [...ARRAY_PROTOTYPE_METHODS].sort());
assert.deepEqual(byName.get('negative-infinity-vs-zero').changedPaths, ['n']);
assert.deepEqual(byName.get('negative-infinity-same').changedPaths, []);
assert.deepEqual(byName.get('negative-infinity-vs-null').changedPaths, ['n']);
assert.deepEqual(byName.get('inputSchema-own-proto-empty').changedPaths, []);
assert.deepEqual(byName.get('inputSchema-own-proto-x').changedPaths, ['modules.tools.tools[0].inputSchema.__proto__.x']);
assert.deepEqual(byName.get('inputSchema-own-proto-x').changeClasses, ['restart-required']);

const sourceSha256 = { [source]: createHash('sha256').update(await readFile(join(baseline, source))).digest('hex') };
const oracle = JSON.stringify({ sourceCommit, sourceSha256, pairs, classify: cases }, null, 2) + '\n';
for (const dir of [baseline, output]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), oracle);
console.log(`PASS: recorded ${pairs.length} real TS snapshot pairs and ${cases.length} classify cases`);
