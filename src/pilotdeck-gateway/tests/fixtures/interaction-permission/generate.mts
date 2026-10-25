// Run with the existing baseline tsx loader; no downloads or services.
// PDRS_BASELINE=/abs/PilotDeck node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
//   src/pilotdeck-gateway/tests/fixtures/interaction-permission/generate.mts /new/output/dir
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);
const entries = [
  'src/cli/SessionInteractionBundle.ts',
  'src/interaction/InteractionProfile.ts',
  'src/interaction/InteractionPolicy.ts',
  'src/interaction/ProfiledPermissionDecisionPort.ts',
  'src/permission/decision/PermissionRuntime.ts',
  'src/gateway/permission/createGatewayPermissionHook.ts',
  'src/permission/protocol/types.ts',
  'src/gateway/permission/GatewayPermissionBus.ts',
  'src/gateway/elicitation/GatewayElicitationBus.ts',
];
const hashes = await baselineSourceHashes(baseline, entries);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const { SessionInteractionBundle } = await load(entries[0]);
const { resolveInteractionProfile } = await load(entries[1]);
const { createDefaultPermissionContext } = await load(entries[6]);
const { createGatewayPermissionHook } = await load(entries[5]);
const { GatewayPermissionBus } = await load(entries[7]);
const { GatewayElicitationBus } = await load(entries[8]);

const profiles = [];
for (const [name, canPrompt, forcedMcp, policyCanPrompt] of [
  ['interactive', true, false, undefined],
  ['headless', true, false, undefined],
  ['disabled', true, false, undefined],
  ['disabled', false, false, undefined],
  ['interactive', false, false, undefined],
  ['interactive', false, true, undefined],
  ['disabled', false, true, undefined],
  ['headless', true, false, false],
] as const) {
  let emitted = 0;
  const disposers: Array<() => unknown> = [];
  const profile = resolveInteractionProfile(name);
  const permissionContext = createDefaultPermissionContext({
    cwd: '/workspace', canPrompt, policyCanPrompt,
    ...(forcedMcp ? { rules: { ask: [
      { source: 'session', behavior: 'ask', toolName: 'mcp__danger', force: true },
    ] } } : {}),
  });
  const bundle = new SessionInteractionBundle({
    sessionKey: 's', profile, canPrompt: profile.canPrompt && canPrompt,
    permissionRules: permissionContext.rules.allow, hookSettings: {},
    shell: { execute: () => { throw new Error('probe must not execute shell'); } },
    projectRoot: '/workspace', gateway: {
      permissionBus: new GatewayPermissionBus(), elicitationBus: new GatewayElicitationBus(),
      interactionReconnect: {}, emit: () => { emitted++; return false; },
    },
  });
  bundle.attach({ own(dispose: () => unknown) { disposers.push(dispose); } });
  const tool = { name: forcedMcp ? 'mcp__danger' : 'danger', isReadOnly: () => false };
  const result = await bundle.permission.decide(tool, null, { permissionContext }, 'call');
  profiles.push({ name, canPrompt, forcedMcp, policyCanPrompt: policyCanPrompt ?? null,
    result, emitted, registrations: disposers.length });
  for (const dispose of disposers.reverse()) await dispose();
}

const hooks = [];
for (const mode of ['allow', 'no-sink', 'cancel', 'throw-error', 'throw-string', 'answer-then-throw', 'answer-then-no-sink']) {
  const bus = new GatewayPermissionBus();
  const rules: unknown[] = [];
  const abort = new AbortController();
  if (mode === 'cancel') abort.abort();
  const hook = createGatewayPermissionHook({ sessionKey: 's', bus, permissionRules: rules,
    uuid: () => 'request', emit: (event: any) => {
      if (mode === 'no-sink') return false;
      if (mode === 'throw-error') throw new Error('sink failed');
      if (mode === 'throw-string') throw 'sink failed';
      bus.consume('s', event.requestId)?.resolve({ requestId: event.requestId, decision: 'allow', remember: true });
      if (mode === 'answer-then-throw') throw new Error('after answer');
      return mode !== 'answer-then-no-sink';
    },
  });
  const result = await hook({ hookInput: { toolName: 'danger', toolCallId: 'call', toolInput: null }, signal: abort.signal });
  hooks.push({ mode, result, rules });
}
const byMode = new Map(hooks.map(row => [row.mode, row]));
const allow = byMode.get('allow')!;
const answerThenNoSink = byMode.get('answer-then-no-sink')!;
assert.deepEqual(answerThenNoSink.result, allow.result,
  'a synchronous answer must win over a later delivery=false');
assert.deepEqual(answerThenNoSink.rules, allow.rules);
for (const mode of ['throw-error', 'throw-string']) {
  assert.deepEqual(byMode.get(mode)!.result, {
    type: 'sync', specific: { hookEventName: 'PermissionRequest', decision: {
      behavior: 'deny', message: 'Permission prompt delivery failed: sink failed',
    } },
  });
  assert.deepEqual(byMode.get(mode)!.rules, []);
}
assertBaseline(baseline);
await mkdir(output); // Never overwrite prior evidence.
for (const [name, value] of Object.entries({ profiles, oracle: hooks.slice(0, 3), hooks, source: { commit, hashes } })) {
  await writeFile(join(output, `${name}.json`), JSON.stringify(value, null, 2) + '\n', { flag: 'wx' });
}
console.log(JSON.stringify({ output, profiles, hooks }, null, 2));
