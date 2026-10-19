import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Run with Node 22 --expose-gc --experimental-strip-types; see README.md.
// Every expected value is produced by the real TypeScript baseline classes.
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);

const sources = await baselineSourceHashes(baseline, [
  'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts',
  'src/gateway/permission/GatewaySessionPermissionModeRegistry.ts',
  'src/always-on/runtime/SessionConfigOverrides.ts',
]);

const { GatewaySessionPermissionRuleSetRegistry } = await import(
  pathToFileURL(join(baseline, 'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts')).href
);
const { GatewaySessionPermissionModeRegistry } = await import(
  pathToFileURL(join(baseline, 'src/gateway/permission/GatewaySessionPermissionModeRegistry.ts')).href
);
const { SessionConfigOverrides, UNATTENDED_SESSION_EXCLUDED_TOOLS } = await import(
  pathToFileURL(join(baseline, 'src/always-on/runtime/SessionConfigOverrides.ts')).href
);

// JSON has no undefined/NaN/Infinity/-0. Encode those as {"$":tag}; real fixture
// objects must not use the reserved "$" key.
function encode(value: any): any {
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
    const encoded: Record<string, any> = {};
    for (const key of Object.keys(value)) {
      assert.notEqual(key, '$', 'fixture object uses the reserved "$" key');
      encoded[key] = encode(value[key]);
    }
    return encoded;
  }
  return value;
}

const rule = (source: string, behavior: string, toolName: string, extra: Record<string, any> = {}) => ({
  source,
  behavior,
  toolName,
  ...extra,
});
const rules = (value: any) => (Array.isArray(value) ? value : []);
// Keep the raw values here: the outer `encode` is the single boundary that
// turns `undefined`/NaN/... into tags. Encoding here too would make a legal
// `pattern: undefined` look like a reserved-key object on the second pass.
const ruleSet = (value: any) => ({
  allow: rules(value?.allow),
  deny: rules(value?.deny),
  ask: rules(value?.ask),
});

// --- Rule-set registry -------------------------------------------------------
const ruleSetSteps: any[] = [];
const ruleSetRegistry = new GatewaySessionPermissionRuleSetRegistry();
const leases = new Map<string, any>();
const ruleSetKeys: string[] = [];
function noteRuleSetKey(step: any) {
  if (typeof step.key === 'string' && !ruleSetKeys.includes(step.key)) ruleSetKeys.push(step.key);
}
function ruleSetState() {
  const allow: any = {};
  for (const key of ruleSetKeys) allow[key] = ruleSetRegistry.allowRules(key);
  const live: any = {};
  for (const [label, lease] of leases) live[label] = ruleSet(lease.rules);
  return { allow, leases: live };
}
function recordRuleSet(step: any, outcome: any) {
  ruleSetSteps.push({
    step: encode(step),
    outcome: encode(outcome),
    size: ruleSetRegistry.size,
    state: encode(ruleSetState()),
  });
}
function runRuleSet(step: any) {
  noteRuleSetKey(step);
  switch (step.op) {
    case 'size':
      recordRuleSet(step, ruleSetRegistry.size);
      return;
    case 'acquire': {
      const lease = ruleSetRegistry.acquire(step.key, step.configured);
      leases.set(step.lease, lease);
      recordRuleSet(step, { rules: ruleSet(lease.rules) });
      return;
    }
    case 'release':
      leases.get(step.lease).release();
      recordRuleSet(step, { released: true });
      return;
    case 'leaseRules':
      recordRuleSet(step, { rules: ruleSet(leases.get(step.lease).rules) });
      return;
    case 'grant':
      recordRuleSet(step, ruleSetRegistry.grant(step.key, step.rule));
      return;
    case 'allow':
      recordRuleSet(step, ruleSetRegistry.allowRules(step.key));
      return;
    case 'updateAsk': {
      let before: any = null;
      let after: any = null;
      const updated = ruleSetRegistry.updateAskRules(step.key, (current: any[]) => {
        before = [...current];
        for (const added of step.push) current.push(added);
        after = [...current];
      });
      recordRuleSet(step, { updated, askBefore: before, askAfter: after });
      return;
    }
    case 'close':
      ruleSetRegistry.closeSession(step.key);
      recordRuleSet(step, {});
      return;
    case 'dispose':
      ruleSetRegistry.dispose();
      recordRuleSet(step, {});
      return;
    default:
      throw new Error(`unknown rule-set step ${step.op}`);
  }
}

const r1 = rule('user', 'allow', 'Read');
const r2 = rule('project', 'allow', 'Write', { pattern: 'src/**' });
const r3 = rule('project', 'deny', 'Bash');
const r4 = rule('policy', 'ask', 'Bash');
const r5 = rule('user', 'allow', 'Read', { force: true });
const r6 = rule('user', 'allow', 'Read', { pattern: 'src/*.rs' });
const r7 = rule('session', 'allow', 'Grep');
const r8 = rule('project', 'deny', 'Edit');
const r9 = rule('cli', 'ask', 'WebFetch');
const invalidRule = rule('not-a-source', 'not-a-behavior', '', { pattern: '' });
const surrogateRule = rule('\ud800', 'allow', '\ud800', { pattern: '\ud800' });
// Explicit `undefined` optional members are legal baseline input and compare
// like omitted members. They must survive the outer encoder exactly once.
const undefinedPatternRule = rule('user', 'allow', 'Read', { pattern: undefined });
const undefinedForceRule = rule('project', 'deny', 'Bash', { force: undefined });
const nullPatternRule = rule('user', 'allow', 'Read', { pattern: null });

const ruleSetScenario = [
  { op: 'size' },
  { op: 'allow', key: 'missing' },
  { op: 'updateAsk', key: 'missing', push: [r4] },
  { op: 'acquire', lease: 'L1', key: 's1', configured: { allow: [r1, r2], deny: [r3], ask: [r4] } },
  { op: 'leaseRules', lease: 'L1' },
  { op: 'grant', key: 's1', rule: r1 },
  { op: 'grant', key: 's1', rule: r5 },
  { op: 'grant', key: 's1', rule: r6 },
  { op: 'grant', key: 's1', rule: r6 },
  { op: 'allow', key: 's1' },
  { op: 'leaseRules', lease: 'L1' },
  // Overlapping recreate folds new rules into the same arrays without duplicates.
  { op: 'acquire', lease: 'L2', key: 's1', configured: { allow: [r7, r1, r7], deny: [r3, r8], ask: [r4, r9] } },
  { op: 'release', lease: 'L1' },
  { op: 'release', lease: 'L1' },
  { op: 'leaseRules', lease: 'L2' },
  { op: 'release', lease: 'L2' },
  { op: 'close', key: 's1' },
  { op: 'allow', key: 's1' },
  // Released leases keep the exact arrays even after the map entry is gone.
  { op: 'leaseRules', lease: 'L1' },
  // A grant may arrive before an AgentHandle; closeSession clears the pin.
  { op: 'grant', key: 's2', rule: invalidRule },
  { op: 'close', key: 's2' },
  { op: 'close', key: 's2' },
  { op: 'allow', key: 's2' },
  // A pinned entry survives a lease release and is removed by closeSession.
  { op: 'grant', key: 's3', rule: r1 },
  { op: 'grant', key: 's3', rule: r1 },
  { op: 'acquire', lease: 'L3', key: 's3' },
  { op: 'release', lease: 'L3' },
  { op: 'close', key: 's3' },
  // Creation keeps configured duplicates; merge never adds one twice.
  { op: 'acquire', lease: 'L4', key: 's4', configured: { allow: [r1, r1, r2], deny: [], ask: [] } },
  { op: 'grant', key: 's4', rule: r1 },
  { op: 'grant', key: 's4', rule: r2 },
  { op: 'updateAsk', key: 's4', push: [r4] },
  { op: 'leaseRules', lease: 'L4' },
  { op: 'release', lease: 'L4' },
  { op: 'close', key: 's4' },
  // Explicit `undefined` optional fields round-trip once and compare like
  // omitted fields, so both grants below are duplicates.
  {
    op: 'acquire',
    lease: 'U1',
    key: 's5',
    configured: { allow: [undefinedPatternRule], deny: [undefinedForceRule], ask: [] },
  },
  { op: 'grant', key: 's5', rule: r1 },
  { op: 'grant', key: 's5', rule: r3 },
  { op: 'grant', key: 's5', rule: undefinedPatternRule },
  { op: 'grant', key: 's5', rule: undefinedForceRule },
  { op: 'leaseRules', lease: 'U1' },
  { op: 'release', lease: 'U1' },
  { op: 'close', key: 's5' },
  // Empty and unpaired-surrogate session keys are ordinary Map keys.
  { op: 'acquire', lease: 'L5', key: '', configured: { allow: [], deny: [], ask: [] } },
  { op: 'release', lease: 'L5' },
  { op: 'grant', key: '\ud800', rule: surrogateRule },
  { op: 'dispose' },
  { op: 'allow', key: '\ud800' },
  { op: 'dispose' },
  // A lease retained across dispose releases against an already-cleared registry.
  { op: 'acquire', lease: 'L6', key: 's6' },
  { op: 'dispose' },
  { op: 'release', lease: 'L6' },
  { op: 'leaseRules', lease: 'L6' },
  // `null` members are `?? []` like missing/undefined, and `mergeRules(target,
  // null)` is a no-op on an existing entry (#290).
  { op: 'acquire', lease: 'N1', key: 's7', configured: { allow: null, deny: [r3], ask: null } },
  { op: 'grant', key: 's7', rule: r1 },
  { op: 'leaseRules', lease: 'N1' },
  { op: 'acquire', lease: 'N2', key: 's7', configured: { allow: null, deny: null, ask: null } },
  { op: 'leaseRules', lease: 'N2' },
  { op: 'release', lease: 'N1' },
  { op: 'release', lease: 'N2' },
  { op: 'close', key: 's7' },
  // `pattern: null` is compared with ===, so it differs from a missing or
  // undefined pattern but duplicates another null pattern.
  { op: 'acquire', lease: 'N3', key: 's8', configured: { allow: [nullPatternRule] } },
  { op: 'grant', key: 's8', rule: r1 },
  { op: 'grant', key: 's8', rule: nullPatternRule },
  { op: 'grant', key: 's8', rule: undefinedPatternRule },
  { op: 'leaseRules', lease: 'N3' },
  { op: 'release', lease: 'N3' },
  { op: 'close', key: 's8' },
  { op: 'size' },
];
for (const step of ruleSetScenario) runRuleSet(step);

// --- Mode registry -----------------------------------------------------------
const modeSteps: any[] = [];
const modeRegistry = new GatewaySessionPermissionModeRegistry();
const modeKeys: string[] = [];
function noteModeKey(step: any) {
  if (typeof step.key === 'string' && !modeKeys.includes(step.key)) modeKeys.push(step.key);
}
function modeState() {
  const state: any = {};
  for (const key of modeKeys) state[key] = { current: modeRegistry.get(key), base: modeRegistry.getBase(key) };
  return state;
}
function runMode(step: any) {
  noteModeKey(step);
  let outcome: any = {};
  switch (step.op) {
    case 'get':
      outcome = modeRegistry.get(step.key);
      break;
    case 'getBase':
      outcome = modeRegistry.getBase(step.key);
      break;
    case 'set':
      modeRegistry.set(step.key, step.mode);
      break;
    case 'transition':
      modeRegistry.transition(step.key, step.mode, step.fallbackBase);
      break;
    case 'clear':
      modeRegistry.clear(step.key);
      break;
    case 'dispose':
      modeRegistry.dispose();
      break;
    default:
      throw new Error(`unknown mode step ${step.op}`);
  }
  modeSteps.push({ step: encode(step), outcome: encode(outcome), state: encode(modeState()) });
}

const modeScenario = [
  { op: 'get', key: 'm1' },
  { op: 'getBase', key: 'm1' },
  { op: 'set', key: 'm1', mode: 'plan' },
  { op: 'getBase', key: 'm1' },
  { op: 'transition', key: 'm1', mode: 'bypassPermissions' },
  { op: 'transition', key: 'm1', mode: 'plan', fallbackBase: 'default' },
  { op: 'getBase', key: 'm1' },
  { op: 'transition', key: 'm1', mode: 'plan', fallbackBase: 'default' },
  { op: 'getBase', key: 'm1' },
  { op: 'set', key: 'm1', mode: 'default' },
  { op: 'getBase', key: 'm1' },
  { op: 'set', key: 'm1', mode: 'plan' },
  { op: 'transition', key: 'm1', mode: 'default' },
  { op: 'transition', key: 'm1', mode: 'plan', fallbackBase: 'bypassPermissions' },
  { op: 'getBase', key: 'm1' },
  { op: 'transition', key: 'm2', mode: 'plan', fallbackBase: 'bypassPermissions' },
  { op: 'getBase', key: 'm2' },
  { op: 'transition', key: 'm2', mode: 'plan', fallbackBase: 'default' },
  { op: 'getBase', key: 'm2' },
  { op: 'clear', key: 'm1' },
  { op: 'get', key: 'm1' },
  { op: 'getBase', key: 'm1' },
  { op: 'clear', key: 'missing' },
  // The baseline stores out-of-domain modes verbatim.
  { op: 'set', key: 'm1', mode: 'turbo' },
  { op: 'set', key: 'm2', mode: '' },
  { op: 'set', key: '', mode: 'plan' },
  { op: 'get', key: '\ud800' },
  { op: 'set', key: '\ud800', mode: 'custom' },
  { op: 'dispose' },
  { op: 'get', key: 'm2' },
  { op: 'getBase', key: '' },
];
for (const step of modeScenario) runMode(step);

// --- Session config overrides ------------------------------------------------
const overrideSteps: any[] = [];
const overrides = new SessionConfigOverrides();
const overrideKeys: string[] = [];
function noteOverrideKey(step: any) {
  if (typeof step.key === 'string' && !overrideKeys.includes(step.key)) overrideKeys.push(step.key);
}
function overrideState() {
  const state: any = {};
  for (const key of overrideKeys) state[key] = overrides.get(key);
  return state;
}
function runOverride(step: any) {
  noteOverrideKey(step);
  let outcome: any = {};
  switch (step.op) {
    case 'set':
      overrides.set(step.key, step.value);
      break;
    case 'setMutate': {
      const input = structuredClone(step.value);
      overrides.set(step.key, input);
      for (const [key, value] of Object.entries(step.mutate)) input[key] = value;
      outcome = overrides.get(step.key);
      break;
    }
    case 'getMutate': {
      const returned = overrides.get(step.key);
      for (const [key, value] of Object.entries(step.mutate)) returned[key] = value;
      outcome = overrides.get(step.key);
      break;
    }
    case 'get':
      outcome = overrides.get(step.key);
      break;
    case 'delete':
      overrides.delete(step.key);
      break;
    case 'deletePrefix':
      overrides.deletePrefix(step.prefix);
      break;
    case 'clear':
      overrides.clear();
      break;
    default:
      throw new Error(`unknown override step ${step.op}`);
  }
  overrideSteps.push({ step: encode(step), outcome: encode(outcome), state: encode(overrideState()) });
}

const fullOverride = {
  cwd: '/work',
  permissionMode: 'default',
  bypassAvailable: true,
  canPrompt: false,
  permissionRules: { allow: [r1], deny: [], ask: [] },
  excludeTools: ['enter_plan_mode', 'ask_user_question'],
};
const overrideScenario = [
  { op: 'get', key: 'o1' },
  { op: 'set', key: 'o1', value: fullOverride },
  { op: 'get', key: 'o1' },
  // Same key is replaced, not merged.
  { op: 'set', key: 'o1', value: { cwd: '/replaced' } },
  { op: 'get', key: 'o1' },
  // Unknown properties and out-of-domain modes survive the spread.
  { op: 'set', key: 'o2', value: { cwd: '/extra', extra: { nested: 1 }, permissionMode: 'turbo' } },
  { op: 'get', key: 'o2' },
  // Object spread of null/undefined/primitives.
  { op: 'set', key: 'o3', value: null },
  { op: 'set', key: 'o4', value: undefined },
  { op: 'set', key: 'o5', value: 'ab' },
  { op: 'set', key: 'o6', value: 42 },
  { op: 'set', key: 'o7', value: ['x', 'y'] },
  { op: 'get', key: 'o3' },
  { op: 'get', key: 'o4' },
  { op: 'get', key: 'o5' },
  { op: 'get', key: 'o6' },
  { op: 'get', key: 'o7' },
  // A surrogate pair contributes two UTF-16 index properties, like JS strings.
  { op: 'set', key: 'o8', value: '\ud83d\ude00' },
  { op: 'get', key: 'o8' },
  // Mutating the input after set, or the top-level object returned by get,
  // must not change the stored entry.
  { op: 'setMutate', key: 'o9', value: { cwd: '/input', keep: true }, mutate: { cwd: '/mutated', added: 1 } },
  { op: 'getMutate', key: 'o9', mutate: { cwd: '/returned', extra: true } },
  { op: 'get', key: 'o9' },
  { op: 'delete', key: 'o1' },
  { op: 'delete', key: 'missing' },
  { op: 'get', key: 'o1' },
  // deletePrefix removes every matching key and leaves non-matching keys alone.
  { op: 'set', key: 'p1', value: { cwd: '/p1' } },
  { op: 'set', key: 'p2', value: { cwd: '/p2' } },
  { op: 'set', key: 'px', value: { cwd: '/px' } },
  { op: 'set', key: 'q1', value: { cwd: '/q1' } },
  { op: 'deletePrefix', prefix: 'p' },
  { op: 'get', key: 'p1' },
  { op: 'get', key: 'px' },
  { op: 'get', key: 'q1' },
  { op: 'set', key: '', value: { cwd: '/empty' } },
  { op: 'set', key: '\ud800', value: { cwd: '/surrogate' } },
  { op: 'clear' },
  { op: 'get', key: 'q1' },
  { op: 'get', key: '' },
  { op: 'get', key: '\ud800' },
  // deletePrefix boundaries (#290): a prefix longer than a key, a prefix equal
  // to a whole key (and shorter than another), a lone surrogate prefix, and
  // the empty prefix, which every key startsWith and so clears the store.
  { op: 'set', key: 'ab', value: { cwd: '/ab' } },
  { op: 'set', key: 'abc', value: { cwd: '/abc' } },
  { op: 'set', key: 'b', value: { cwd: '/b' } },
  { op: 'set', key: '\ud800x', value: { cwd: '/surrogate-x' } },
  { op: 'deletePrefix', prefix: 'abcd' },
  { op: 'get', key: 'abc' },
  { op: 'deletePrefix', prefix: 'ab' },
  { op: 'get', key: 'ab' },
  { op: 'get', key: 'abc' },
  { op: 'get', key: 'b' },
  { op: 'deletePrefix', prefix: '\ud800' },
  { op: 'get', key: '\ud800x' },
  { op: 'set', key: '', value: { cwd: '/empty-again' } },
  { op: 'deletePrefix', prefix: '' },
  { op: 'get', key: '' },
  { op: 'get', key: 'b' },
  { op: 'get', key: 'abc' },
];
for (const step of overrideScenario) runOverride(step);

// --- Combined: overrides ↔ live rule set ------------------------------------
// The factory reads `SessionConfigOverrides.get(key).permissionRules` and hands
// it to `acquire` (ProjectSessionFactory.ts:263), so the override's arrays and
// the registry's live arrays are the same objects. These steps exercise that
// alias, not the two classes in isolation.
const combinedSteps: any[] = [];
const combinedOverrides = new SessionConfigOverrides();
const combinedRegistry = new GatewaySessionPermissionRuleSetRegistry();
const combinedLeases = new Map<string, any>();
const combinedAllowViews = new Map<string, readonly any[]>();
const combinedKeys: string[] = [];
function noteCombinedKey(step: any) {
  if (typeof step.key === 'string' && !combinedKeys.includes(step.key)) combinedKeys.push(step.key);
}
function combinedState() {
  const overridesState: any = {};
  for (const key of combinedKeys) overridesState[key] = combinedOverrides.get(key);
  const allow: any = {};
  for (const key of combinedKeys) allow[key] = combinedRegistry.allowRules(key);
  const held: any = {};
  for (const [label, rules] of combinedAllowViews) held[label] = rules;
  const live: any = {};
  for (const [label, lease] of combinedLeases) live[label] = ruleSet(lease.rules);
  return { overrides: overridesState, allow, held, leases: live };
}
function recordCombined(step: any, outcome: any) {
  combinedSteps.push({
    step: encode(step),
    outcome: encode(outcome),
    size: combinedRegistry.size,
    state: encode(combinedState()),
  });
}
function runCombined(step: any) {
  noteCombinedKey(step);
  switch (step.op) {
    case 'set':
      combinedOverrides.set(step.key, step.value);
      recordCombined(step, {});
      return;
    case 'get':
      recordCombined(step, combinedOverrides.get(step.key));
      return;
    case 'setFromGet': {
      // The real cwd/mode update path: get, shallow-spread the existing
      // override, set. The nested permissionRules object must stay shared.
      const existing = combinedOverrides.get(step.key) ?? {};
      combinedOverrides.set(step.key, { ...existing, ...step.patch });
      recordCombined(step, combinedOverrides.get(step.key));
      return;
    }
    case 'acquire': {
      const configured = combinedOverrides.get(step.key)?.permissionRules;
      const lease = combinedRegistry.acquire(step.key, configured);
      // The baseline adopts the configured arrays by reference on first
      // acquire; the fixture would be meaningless if this identity broke.
      if (configured && Array.isArray(configured.allow)) {
        assert.equal(lease.rules.allow, configured.allow, 'acquire must adopt configured allow array');
      }
      combinedLeases.set(step.lease, lease);
      recordCombined(step, { rules: ruleSet(lease.rules) });
      return;
    }
    case 'leaseRules':
      recordCombined(step, { rules: ruleSet(combinedLeases.get(step.lease).rules) });
      return;
    case 'grant':
      recordCombined(step, combinedRegistry.grant(step.key, step.rule));
      return;
    case 'allow':
      recordCombined(step, combinedRegistry.allowRules(step.key));
      return;
    case 'holdAllow':
      combinedAllowViews.set(step.view, combinedRegistry.allowRules(step.key));
      recordCombined(step, {});
      return;
    case 'readHeldAllow': {
      const held = combinedAllowViews.get(step.view);
      if (!held) throw new Error(`unknown held allow view ${step.view}`);
      recordCombined(step, held);
      return;
    }
    case 'updateAsk': {
      let askBefore: any = null;
      let askAfter: any = null;
      const updated = combinedRegistry.updateAskRules(step.key, (current: any[]) => {
        askBefore = [...current];
        for (const added of step.push) current.push(added);
        askAfter = [...current];
      });
      recordCombined(step, { updated, askBefore, askAfter });
      return;
    }
    case 'close':
      combinedRegistry.closeSession(step.key);
      recordCombined(step, {});
      return;
    case 'release':
      combinedLeases.get(step.lease).release();
      recordCombined(step, { released: true });
      return;
    case 'dispose':
      combinedRegistry.dispose();
      recordCombined(step, {});
      return;
    default:
      throw new Error(`unknown combined step ${step.op}`);
  }
}

const combinedScenario = [
  // The sequence reported in review round 1: override with an empty allow
  // array → acquire → grant → close → release → recreate → read.
  { op: 'get', key: 'c1' },
  { op: 'set', key: 'c1', value: { cwd: '/c1', permissionRules: { allow: [], deny: [], ask: [] } } },
  { op: 'acquire', lease: 'C1', key: 'c1' },
  // Hold the live allow view before the grant, the way the Gateway does across
  // its awaits; the later grant must show up when the held view is read.
  { op: 'holdAllow', view: 'V1', key: 'c1' },
  { op: 'grant', key: 'c1', rule: r1 },
  { op: 'readHeldAllow', view: 'V1' },
  // A cwd update during the live session (get → spread → set) must not detach
  // the override from the registry arrays.
  { op: 'setFromGet', key: 'c1', patch: { cwd: '/c1-renamed' } },
  { op: 'grant', key: 'c1', rule: r8 },
  { op: 'get', key: 'c1' },
  // The override view and the lease share the same live allow array.
  { op: 'get', key: 'c1' },
  { op: 'allow', key: 'c1' },
  { op: 'leaseRules', lease: 'C1' },
  { op: 'close', key: 'c1' },
  { op: 'release', lease: 'C1' },
  // The factory re-reads override.permissionRules on recreate: the grant stayed.
  { op: 'acquire', lease: 'C2', key: 'c1' },
  { op: 'leaseRules', lease: 'C2' },
  { op: 'get', key: 'c1' },
  // A grant through the replacement generation reaches every holder.
  { op: 'grant', key: 'c1', rule: r6 },
  { op: 'leaseRules', lease: 'C1' },
  { op: 'leaseRules', lease: 'C2' },
  { op: 'get', key: 'c1' },
  // updateAskRules mutates the same ask array observed by the override and by
  // both leases.
  { op: 'updateAsk', key: 'c1', push: [r4] },
  { op: 'leaseRules', lease: 'C1' },
  { op: 'leaseRules', lease: 'C2' },
  { op: 'get', key: 'c1' },
  { op: 'close', key: 'c1' },
  { op: 'release', lease: 'C2' },

  // A stale lease from an older generation must not delete the replacement
  // entry, and the replacement still sees the override's live arrays.
  { op: 'dispose' },
  { op: 'set', key: 'c2', value: { permissionRules: { allow: [r2] } } },
  { op: 'acquire', lease: 'C3', key: 'c2' },
  { op: 'dispose' },
  { op: 'acquire', lease: 'C4', key: 'c2' },
  { op: 'release', lease: 'C3' },
  { op: 'leaseRules', lease: 'C4' },
  { op: 'allow', key: 'c2' },
  { op: 'grant', key: 'c2', rule: r7 },
  { op: 'leaseRules', lease: 'C4' },
  { op: 'get', key: 'c2' },
  { op: 'close', key: 'c2' },
  { op: 'release', lease: 'C4' },

  // A partial configured object only exposes the members it declared.
  { op: 'set', key: 'c3', value: { permissionRules: { allow: [r1, r1] } } },
  { op: 'acquire', lease: 'C5', key: 'c3' },
  { op: 'leaseRules', lease: 'C5' },
  { op: 'get', key: 'c3' },
  { op: 'release', lease: 'C5' },
  { op: 'close', key: 'c3' },

  // Explicit `undefined` optional members survive the override → handle →
  // registry path and still compare like omitted members.
  {
    op: 'set',
    key: 'c4',
    value: {
      permissionRules: {
        allow: [rule('user', 'allow', 'Read', { pattern: undefined })],
        deny: [rule('project', 'deny', 'Bash', { force: undefined })],
      },
    },
  },
  { op: 'acquire', lease: 'C6', key: 'c4' },
  { op: 'grant', key: 'c4', rule: r1 },
  { op: 'grant', key: 'c4', rule: undefinedPatternRule },
  { op: 'leaseRules', lease: 'C6' },
  { op: 'get', key: 'c4' },
  { op: 'release', lease: 'C6' },
  { op: 'close', key: 'c4' },
];
for (const step of combinedScenario) runCombined(step);

// --- Object identity: `permissionRules` views across stores and deletes ----
// The baseline's `set`/`get` shallow-copy only the top level, so the nested
// `permissionRules` object and its declared arrays keep their identity. These
// steps pin that identity down; the Rust port cannot observe it from JSON
// alone, so `sameStores` / `sameHeld` record the reference comparison.
const identitySteps: any[] = [];
const identityStores = new Map<string, SessionConfigOverrides>();
const identityRegistry = new GatewaySessionPermissionRuleSetRegistry();
const identityLeases = new Map<string, any>();
const identityViews = new Map<string, any>();
const identityHeldRules = new Map<string, any>();
const identityStoreKeys: Array<[string, string]> = [];
const identitySessionKeys: string[] = [];
function identityStore(name: string) {
  let store = identityStores.get(name);
  if (!store) {
    store = new SessionConfigOverrides();
    identityStores.set(name, store);
  }
  return store;
}
function noteIdentityKey(step: any) {
  if (typeof step.store === 'string' && typeof step.key === 'string' && !identityStoreKeys.some(([s, k]) => s === step.store && k === step.key)) {
    identityStoreKeys.push([step.store, step.key]);
  }
  if (typeof step.session === 'string' && !identitySessionKeys.includes(step.session)) {
    identitySessionKeys.push(step.session);
  }
}
function identityState() {
  const stores: any = {};
  for (const [store, key] of identityStoreKeys) stores[`${store}:${key}`] = identityStores.get(store)?.get(key);
  const allow: any = {};
  for (const key of identitySessionKeys) allow[key] = identityRegistry.allowRules(key);
  const leases: any = {};
  for (const [label, lease] of identityLeases) leases[label] = ruleSet(lease.rules);
  return { stores, allow, leases };
}
function recordIdentity(step: any, outcome: any) {
  identitySteps.push({
    step: encode(step),
    outcome: encode(outcome),
    size: identityRegistry.size,
    state: encode(identityState()),
  });
}
function runIdentity(step: any) {
  noteIdentityKey(step);
  switch (step.op) {
    case 'set':
      identityStore(step.store).set(step.key, step.value);
      recordIdentity(step, {});
      return;
    case 'get': {
      const view = identityStore(step.store).get(step.key);
      if (step.hold) {
        if (view === undefined) throw new Error(`get(${step.key}) is undefined`);
        identityViews.set(step.hold, view);
        identityHeldRules.set(step.hold, view.permissionRules);
      }
      recordIdentity(step, view ?? undefined);
      return;
    }
    case 'setFromView': {
      const view = identityViews.get(step.view);
      if (view === undefined) throw new Error(`unknown held view ${step.view}`);
      identityStore(step.store).set(step.key, view);
      recordIdentity(step, {});
      return;
    }
    case 'setFromViewPatch': {
      const view = identityViews.get(step.view);
      if (view === undefined) throw new Error(`unknown held view ${step.view}`);
      identityStore(step.store).set(step.key, { ...view, ...step.patch });
      recordIdentity(step, {});
      return;
    }
    case 'sameStores': {
      const left = identityStore(step.left[0]).get(step.left[1])?.permissionRules;
      const right = identityStore(step.right[0]).get(step.right[1])?.permissionRules;
      recordIdentity(step, left !== undefined && right !== undefined && left === right);
      return;
    }
    case 'sameHeld': {
      const current = identityStore(step.store).get(step.key)?.permissionRules;
      const held = identityHeldRules.get(step.view);
      recordIdentity(step, current !== undefined && held !== undefined && current === held);
      return;
    }
    case 'delete':
      identityStore(step.store).delete(step.key);
      recordIdentity(step, {});
      return;
    case 'acquire': {
      const configured = identityStore(step.store).get(step.key)?.permissionRules;
      const lease = identityRegistry.acquire(step.session, configured);
      // The baseline adopts a declared array by reference on first acquire;
      // without this the fixture could not distinguish sharing from copying.
      if (configured && Array.isArray(configured.allow)) {
        assert.equal(lease.rules.allow, configured.allow, 'identity acquire must adopt configured allow');
      }
      identityLeases.set(step.lease, lease);
      recordIdentity(step, { rules: ruleSet(lease.rules) });
      return;
    }
    case 'leaseRules':
      recordIdentity(step, { rules: ruleSet(identityLeases.get(step.lease).rules) });
      return;
    case 'grant':
      recordIdentity(step, identityRegistry.grant(step.session, step.rule));
      return;
    case 'allow':
      recordIdentity(step, identityRegistry.allowRules(step.session));
      return;
    case 'updateAsk': {
      let askBefore: any = null;
      let askAfter: any = null;
      const updated = identityRegistry.updateAskRules(step.session, (current: any[]) => {
        askBefore = [...current];
        for (const added of step.push) current.push(added);
        askAfter = [...current];
      });
      recordIdentity(step, { updated, askBefore, askAfter });
      return;
    }
    case 'close':
      identityRegistry.closeSession(step.session);
      recordIdentity(step, {});
      return;
    case 'release':
      identityLeases.get(step.lease).release();
      recordIdentity(step, { released: true });
      return;
    case 'dispose':
      identityRegistry.dispose();
      recordIdentity(step, {});
      return;
    default:
      throw new Error(`unknown identity step ${step.op}`);
  }
}

const read = rule('session', 'allow', 'Read');
const write = rule('session', 'allow', 'Write');
const bash = rule('session', 'allow', 'Bash');
const grep = rule('session', 'allow', 'Grep');
const askBash = rule('policy', 'ask', 'Bash');
const identityScenario = [
  // Codex sequence 1: `permissionRules: {}` → acquire → grant(Read) →
  // updateAsk → close → release → acquire. TS rebuilds all three missing
  // members as fresh arrays, so the old grant/ask must not survive.
  { op: 'set', store: 'M', key: 'empty', value: { cwd: '/empty', permissionRules: {} } },
  { op: 'acquire', store: 'M', key: 'empty', session: 'empty', lease: 'E1' },
  { op: 'grant', session: 'empty', rule: read },
  { op: 'updateAsk', session: 'empty', push: [askBash] },
  { op: 'close', session: 'empty' },
  { op: 'release', lease: 'E1' },
  { op: 'acquire', store: 'M', key: 'empty', session: 'empty', lease: 'E2' },
  { op: 'leaseRules', lease: 'E2' },
  { op: 'allow', session: 'empty' },
  { op: 'get', store: 'M', key: 'empty' },
  // Only `allow` is declared: its grant survives, the default `ask` is a new
  // array on the replacement generation.
  { op: 'set', store: 'M', key: 'allowOnly', value: { permissionRules: { allow: [] } } },
  { op: 'acquire', store: 'M', key: 'allowOnly', session: 'allowOnly', lease: 'A1' },
  { op: 'grant', session: 'allowOnly', rule: read },
  { op: 'updateAsk', session: 'allowOnly', push: [askBash] },
  { op: 'close', session: 'allowOnly' },
  { op: 'release', lease: 'A1' },
  { op: 'acquire', store: 'M', key: 'allowOnly', session: 'allowOnly', lease: 'A2' },
  { op: 'leaseRules', lease: 'A2' },
  { op: 'get', store: 'M', key: 'allowOnly' },
  // All three declared: every array is the configured one, so both survive.
  {
    op: 'set',
    store: 'M',
    key: 'all',
    value: { permissionRules: { allow: [], deny: [], ask: [] } },
  },
  { op: 'acquire', store: 'M', key: 'all', session: 'all', lease: 'B1' },
  { op: 'grant', session: 'all', rule: read },
  { op: 'updateAsk', session: 'all', push: [askBash] },
  { op: 'close', session: 'all' },
  { op: 'release', lease: 'B1' },
  { op: 'acquire', store: 'M', key: 'all', session: 'all', lease: 'B2' },
  { op: 'leaseRules', lease: 'B2' },
  // Explicit `undefined` is `?? []`, i.e. a fresh array, not a shared one.
  {
    op: 'set',
    store: 'M',
    key: 'undefinedAllow',
    value: { permissionRules: { allow: undefined } },
  },
  { op: 'acquire', store: 'M', key: 'undefinedAllow', session: 'undefinedAllow', lease: 'U1' },
  { op: 'grant', session: 'undefinedAllow', rule: read },
  { op: 'close', session: 'undefinedAllow' },
  { op: 'release', lease: 'U1' },
  { op: 'acquire', store: 'M', key: 'undefinedAllow', session: 'undefinedAllow', lease: 'U2' },
  { op: 'leaseRules', lease: 'U2' },

  // Codex sequence 2a: two stores, A.a=[Read], B.b=[Write],
  // B.set("c", A.get("a")). C must share A's object, not B's first handle.
  { op: 'set', store: 'A', key: 'a', value: { permissionRules: { allow: [read] } } },
  { op: 'set', store: 'B', key: 'b', value: { permissionRules: { allow: [write] } } },
  { op: 'get', store: 'A', key: 'a', hold: 'aView' },
  { op: 'setFromView', store: 'B', key: 'c', view: 'aView' },
  { op: 'sameStores', left: ['A', 'a'], right: ['B', 'c'] },
  { op: 'acquire', store: 'B', key: 'b', session: 'b', lease: 'B3' },
  { op: 'grant', session: 'b', rule: bash },
  { op: 'get', store: 'B', key: 'b' },
  { op: 'get', store: 'B', key: 'c' },
  { op: 'sameStores', left: ['A', 'a'], right: ['B', 'c'] },

  // Codex sequence 2b: one held `get` view used for two `set`s. The second set
  // must still carry the live handle, including the grant between them.
  {
    op: 'set',
    store: 'A',
    key: 's',
    value: { permissionRules: { allow: [], deny: [], ask: [] } },
  },
  { op: 'get', store: 'A', key: 's', hold: 'sView' },
  { op: 'acquire', store: 'A', key: 's', session: 's', lease: 'S1' },
  { op: 'setFromViewPatch', store: 'A', key: 's', view: 'sView', patch: { cwd: '/one' } },
  { op: 'grant', session: 's', rule: read },
  { op: 'setFromViewPatch', store: 'A', key: 's', view: 'sView', patch: { cwd: '/two' } },
  { op: 'get', store: 'A', key: 's' },
  { op: 'sameHeld', store: 'A', key: 's', view: 'sView' },
  { op: 'leaseRules', lease: 'S1' },

  // Codex sequence 2c: the held view outlives `delete` and is re-used under a
  // different key; the identity and the live grant must survive.
  { op: 'set', store: 'A', key: 'd', value: { permissionRules: { allow: [grep] } } },
  { op: 'get', store: 'A', key: 'd', hold: 'dView' },
  { op: 'delete', store: 'A', key: 'd' },
  { op: 'setFromView', store: 'A', key: 'e', view: 'dView' },
  { op: 'sameHeld', store: 'A', key: 'e', view: 'dView' },
  { op: 'acquire', store: 'A', key: 'e', session: 'e', lease: 'D1' },
  { op: 'grant', session: 'e', rule: read },
  { op: 'get', store: 'A', key: 'e' },
  // `null` members are `?? []` like missing/undefined: fresh arrays (#290).
  {
    op: 'set',
    store: 'M',
    key: 'nullAllow',
    value: { permissionRules: { allow: null, deny: null } },
  },
  { op: 'acquire', store: 'M', key: 'nullAllow', session: 'nullAllow', lease: 'N1' },
  { op: 'grant', session: 'nullAllow', rule: read },
  { op: 'close', session: 'nullAllow' },
  { op: 'release', lease: 'N1' },
  { op: 'acquire', store: 'M', key: 'nullAllow', session: 'nullAllow', lease: 'N2' },
  { op: 'leaseRules', lease: 'N2' },
  { op: 'get', store: 'M', key: 'nullAllow' },
];
for (const step of identityScenario) runIdentity(step);

// --- Fourth round: explicit object graph and production lease mutation ------
const sharingSteps: any[] = [];
const values = new Map<string, any>();
const sharingStores = new Map<string, SessionConfigOverrides>();
const sharingKeys = new Map<string, Set<string>>();
const sharingRegistry = new GatewaySessionPermissionRuleSetRegistry();
const sharingLeases = new Map<string, any>();
const at = (path: string[]) => path.slice(1).reduce((v, p) => v[p], values.get(path[0]));
function sharingStore(name: string) {
  if (!sharingStores.has(name)) sharingStores.set(name, new SessionConfigOverrides());
  return sharingStores.get(name)!;
}
function sharingState() {
  const stores: any = {};
  for (const [name, keys] of sharingKeys) {
    stores[name] = {};
    for (const key of keys) stores[name][key] = sharingStores.get(name)?.get(key);
  }
  return { values: Object.fromEntries(values), stores, leases: Object.fromEntries([...sharingLeases].map(([k, l]) => [k, l.rules])) };
}
function sharing(step: any) {
  let outcome: any = {};
  switch (step.op) {
    case 'value': values.set(step.name, step.value); break;
    case 'alias': values.set(step.name, at(step.source)); break;
    case 'spread': values.set(step.name, { ...at(step.source) }); break;
    case 'setProperty': at(step.target)[step.property] = step.source ? at(step.source) : step.value; break;
    case 'push': at(step.target).push(at(step.source)); break;
    case 'leasePush': sharingLeases.get(step.lease).rules[step.member].push(at(step.source)); break;
    case 'remove': at(step.target).splice(step.index, 1); break;
    case 'storeSet':
      if (!sharingKeys.has(step.store)) sharingKeys.set(step.store, new Set());
      sharingKeys.get(step.store)!.add(step.key);
      sharingStore(step.store).set(step.key, at(step.source)); break;
    case 'storeGet': values.set(step.name, sharingStore(step.store).get(step.key)); break;
    case 'delete': sharingStore(step.store).delete(step.key); break;
    case 'clear': sharingStore(step.store).clear(); break;
    case 'acquire': sharingLeases.set(step.lease, sharingRegistry.acquire(step.key, step.source ? at(step.source) : undefined)); break;
    case 'lease': values.set(step.name, sharingLeases.get(step.lease).rules); break;
    case 'allow': values.set(step.name, sharingRegistry.allowRules(step.key)); break;
    case 'grant': outcome = sharingRegistry.grant(step.key, at(step.source)); break;
    case 'same': outcome = at(step.left) === at(step.right); break;
    case 'release': sharingLeases.get(step.lease).release(); break;
    case 'close': sharingRegistry.closeSession(step.key); break;
    case 'dispose': sharingRegistry.dispose(); break;
    default: throw new Error(`unknown sharing operation ${step.op}`);
  }
  sharingSteps.push({ step: encode(step), outcome: encode(outcome), size: sharingRegistry.size, state: encode(sharingState()) });
}
const sharingScenario = [
  { op: 'value', name: 'input', value: { cwd: '/one', permissionRules: { allow: [] }, excludeTools: ['ask_user_question'], extra: { tag: 'one' } } },
  { op: 'value', name: 'read', value: rule('session', 'allow', 'Read') },
  { op: 'value', name: 'deny', value: rule('session', 'deny', 'Bash') },
  { op: 'storeSet', store: 'O', key: 's', source: ['input'] },
  { op: 'storeSet', store: 'O', key: 't', source: ['input'] },
  { op: 'storeGet', store: 'O', key: 's', name: 'view' },
  { op: 'storeGet', store: 'O', key: 's', name: 'view2' },
  { op: 'same', left: ['input'], right: ['view'] },
  { op: 'same', left: ['view'], right: ['view2'] },
  { op: 'same', left: ['input', 'permissionRules'], right: ['view', 'permissionRules'] },
  { op: 'same', left: ['input', 'excludeTools'], right: ['view', 'excludeTools'] },
  { op: 'setProperty', target: ['input'], property: 'cwd', value: '/changed' },
  { op: 'setProperty', target: ['view', 'extra'], property: 'tag', value: 'two' },
  { op: 'value', name: 'tool', value: 'enter_plan_mode' },
  { op: 'push', target: ['input', 'excludeTools'], source: ['tool'] },
  { op: 'spread', name: 'innerCopy', source: ['input', 'permissionRules'] },
  { op: 'same', left: ['innerCopy'], right: ['input', 'permissionRules'] },
  { op: 'same', left: ['innerCopy', 'allow'], right: ['input', 'permissionRules', 'allow'] },
  { op: 'acquire', key: 's', lease: 'one', source: ['input', 'permissionRules'] },
  { op: 'lease', lease: 'one', name: 'live' },
  { op: 'same', left: ['live'], right: ['input', 'permissionRules'] },
  { op: 'same', left: ['live', 'allow'], right: ['input', 'permissionRules', 'allow'] },
  { op: 'allow', key: 's', name: 'heldAllow' },
  { op: 'grant', key: 's', source: ['read'] },
  { op: 'same', left: ['heldAllow', '0'], right: ['read'] },
  { op: 'setProperty', target: ['read'], property: 'toolName', value: 'ReadChanged' },
  // r3 P1: mutate a default deny through the public lease, share within this
  // generation, release every retain, and get a fresh default deny next time.
  { op: 'leasePush', lease: 'one', member: 'deny', source: ['deny'] },
  { op: 'acquire', key: 's', lease: 'replacement', source: ['innerCopy'] },
  { op: 'lease', lease: 'replacement', name: 'replacementRules' },
  { op: 'same', left: ['live'], right: ['replacementRules'] },
  { op: 'close', key: 's' },
  { op: 'release', lease: 'one' },
  { op: 'release', lease: 'replacement' },
  { op: 'acquire', key: 's', lease: 'fresh', source: ['input', 'permissionRules'] },
  { op: 'lease', lease: 'fresh', name: 'freshRules' },
  { op: 'same', left: ['live'], right: ['freshRules'] },
  { op: 'same', left: ['live', 'deny'], right: ['freshRules', 'deny'] },
  // Assignment changes a container's reference; already held arrays survive.
  { op: 'value', name: 'newArray', value: [] },
  { op: 'setProperty', target: ['input', 'permissionRules'], property: 'allow', source: ['newArray'] },
  { op: 'same', left: ['freshRules', 'allow'], right: ['newArray'] },
  { op: 'acquire', key: 's', lease: 'overlap', source: ['input', 'permissionRules'] },
  { op: 'lease', lease: 'overlap', name: 'overlapRules' },
  { op: 'same', left: ['freshRules'], right: ['overlapRules'] },
  { op: 'value', name: 'leaseArray', value: [] },
  { op: 'setProperty', target: ['freshRules'], property: 'allow', source: ['leaseArray'] },
  { op: 'allow', key: 's', name: 'newAllow' },
  { op: 'same', left: ['newAllow'], right: ['leaseArray'] },
  { op: 'same', left: ['newAllow'], right: ['heldAllow'] },
  { op: 'grant', key: 's', source: ['read'] },
  { op: 'remove', target: ['overlapRules', 'allow'], index: 0 },
  // Late configuration merges rule objects, never adopts the incoming array.
  { op: 'value', name: 'incoming', value: { allow: [rule('user', 'allow', 'Write')] } },
  { op: 'alias', name: 'incomingRule', source: ['incoming', 'allow', '0'] },
  { op: 'acquire', key: 's', lease: 'merged', source: ['incoming'] },
  { op: 'same', left: ['newAllow'], right: ['incoming', 'allow'] },
  { op: 'same', left: ['newAllow', '0'], right: ['incomingRule'] },
  { op: 'setProperty', target: ['incomingRule'], property: 'toolName', value: 'WriteChanged' },
  { op: 'push', target: ['incoming', 'allow'], source: ['read'] },
  // Three aliases to one array must neither split nor deadlock in Rust.
  { op: 'value', name: 'aliased', value: {} },
  { op: 'setProperty', target: ['aliased'], property: 'allow', source: ['newArray'] },
  { op: 'setProperty', target: ['aliased'], property: 'deny', source: ['newArray'] },
  { op: 'setProperty', target: ['aliased'], property: 'ask', source: ['newArray'] },
  { op: 'acquire', key: 'aliased', lease: 'aliasedLease', source: ['aliased'] },
  { op: 'grant', key: 'aliased', source: ['read'] },
  { op: 'acquire', key: 'aliased', lease: 'aliasedAgain', source: ['aliased'] },
  { op: 'lease', lease: 'aliasedLease', name: 'aliasedRules' },
  { op: 'same', left: ['aliasedRules', 'allow'], right: ['aliasedRules', 'ask'] },
  { op: 'allow', key: 'unknown', name: 'unknown1' },
  { op: 'allow', key: 'unknown', name: 'unknown2' },
  { op: 'same', left: ['unknown1'], right: ['unknown2'] },
  // Views keep their nested graph after all store entries are gone.
  { op: 'delete', store: 'O', key: 's' },
  { op: 'clear', store: 'O' },
  { op: 'storeSet', store: 'B', key: 'restored', source: ['view'] },
  { op: 'storeGet', store: 'B', key: 'restored', name: 'restored' },
  { op: 'same', left: ['restored', 'permissionRules'], right: ['input', 'permissionRules'] },
  { op: 'dispose' },
  { op: 'acquire', key: 's', lease: 'afterDispose', source: ['input', 'permissionRules'] },
  { op: 'release', lease: 'fresh' },
  { op: 'release', lease: 'overlap' },
  { op: 'release', lease: 'merged' },
  { op: 'close', key: 's' },
  { op: 'release', lease: 'afterDispose' },
];
for (const step of sharingScenario) sharing(step);

// Mode values are scalars; each write allocates a new private entry. Accessing
// TS-private (not #private) state here is an identity probe, not a reimplementation.
const modeIdentityRegistry = new GatewaySessionPermissionModeRegistry();
modeIdentityRegistry.set('s', 'default');
const modeEntry = (modeIdentityRegistry as any).modes.get('s');
modeIdentityRegistry.transition('s', 'plan');
const modePlan = (modeIdentityRegistry as any).modes.get('s');
modeIdentityRegistry.set('s', 'plan');
const modeIdentity = { transitionCreatesEntry: modeEntry !== modePlan, setCreatesEntry: modePlan !== (modeIdentityRegistry as any).modes.get('s'), getIsScalar: typeof modeIdentityRegistry.get('s') === 'string', baseAfterSet: modeIdentityRegistry.getBase('s') };

// r3 P1: reachability, not a memory-size estimate. Yield before GC because a
// WeakRef keeps its target alive for its current JS job. No input/get view is
// retained in the churn cases. GC failure fails generation, never records PASS.
assert.equal(typeof global.gc, 'function', 'run Node with --expose-gc');
async function collect(weak: WeakRef<any>[]) {
  for (let attempt = 0; attempt < 10; attempt++) {
    await new Promise<void>(resolve => setImmediate(resolve));
    global.gc!();
    if (weak.every(ref => ref.deref() === undefined)) return 0;
  }
  throw new Error('configuration remained reachable after bounded GC');
}
const lifetime: Record<string, number> = {};
for (const operation of ['replace', 'delete', 'deletePrefix', 'clear', 'drop']) {
  const weak: WeakRef<any>[] = [];
  function churn() {
    const store = new SessionConfigOverrides();
    for (let i = 0; i < 100; i++) {
      const array: any[] = [];
      weak.push(new WeakRef(array));
      store.set('s', { permissionRules: { allow: array } });
      if (operation === 'delete') store.delete('s');
      if (operation === 'deletePrefix') store.deletePrefix('s');
      if (operation === 'clear') store.clear();
    }
    if (operation === 'replace') store.set('s', {});
    return operation === 'drop' ? undefined : store;
  }
  const retainedStore = churn();
  lifetime[operation] = await collect(weak);
  // Use the store after GC: delete/clear must release while it is still alive.
  assert.deepEqual(retainedStore?.get('s'), operation === 'replace' ? {} : undefined);
}

// The r1 review also recorded a deliberately out-of-type runtime call. Keep
// its true TS trace here, separately from the typed Rust parity sequences.
const malformedRegistry = new GatewaySessionPermissionRuleSetRegistry();
const malformedFirst = malformedRegistry.grant('bad', null as any);
let malformedSecond: any;
try { malformedRegistry.grant('bad', null as any); }
catch (error: any) { malformedSecond = { name: error.name, message: error.message }; }
assert.equal(malformedFirst, true);
assert.deepEqual(malformedSecond, { name: 'TypeError', message: "Cannot read properties of null (reading 'source')" });
const runtimeBoundary = { firstGrant: malformedFirst, secondGrant: malformedSecond, allow: malformedRegistry.allowRules('bad') };

const oracle = {
  baselineCommit: commit,
  sourceSha256: sources,
  unattendedSessionExcludedTools: UNATTENDED_SESSION_EXCLUDED_TOOLS,
  ruleSet: ruleSetSteps,
  mode: modeSteps,
  overrides: overrideSteps,
  combined: combinedSteps,
  identity: identitySteps,
  sharing: sharingSteps,
  modeIdentity: encode(modeIdentity),
  lifetime,
  runtimeBoundary: encode(runtimeBoundary),
};
const text = JSON.stringify(oracle, null, 2) + '\n';
assert.ok(!text.includes(baseline), 'oracle leaks the baseline path');
assert.ok(!text.includes(output), 'oracle leaks the output path');
await mkdir(output, { recursive: true });
const temporary = await mkdtemp(join(output, '.generate-'));
try {
  const file = join(temporary, 'oracle.json');
  await writeFile(file, text);
  await rename(file, join(output, 'oracle.json'));
} finally {
  await rm(temporary, { recursive: true, force: true });
}
console.log(
  `ruleSet=${ruleSetSteps.length} mode=${modeSteps.length} overrides=${overrideSteps.length} combined=${combinedSteps.length} identity=${identitySteps.length} sharing=${sharingSteps.length} lifetime=${JSON.stringify(lifetime)} bytes=${Buffer.byteLength(text)}`,
);
