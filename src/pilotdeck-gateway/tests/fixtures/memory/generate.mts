// Run from the repository root with the fixed baseline's tsx loader:
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/memory/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/memory
//
// Expectations are produced by the real fixed baseline. The bundle cases run
// the real ProjectMemoryBundle; the projection cases execute the unmodified
// memoryList/memoryWipe method bodies extracted from createLocalGateway.ts with
// an injected fake project registry and provider. No baseline file is copied or
// changed.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

// Shared provenance guard: pinned HEAD, clean tracked tree, and every tracked
// src byte compared against the Git blob at the pinned commit.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
);
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);

const sourceFiles = [
  'src/cli/ProjectMemoryBundle.ts',
  'src/cli/createLocalGateway.ts',
  'src/gateway/dialog/errors.ts',
];
const sourceSha256: Record<string, string> = {};
for (const path of sourceFiles) {
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  const working = await readFile(join(baseline, path));
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

const { ProjectMemoryBundle } = await import(
  pathToFileURL(join(baseline, 'src/cli/ProjectMemoryBundle.ts')).href
);
const { DialogGatewayError } = await import(
  pathToFileURL(join(baseline, 'src/gateway/dialog/errors.ts')).href
);

const PROJECT = '/pdrs-fixture/project';
const FALLBACK = '/pdrs-fixture/fallback';

function errorJson(error: any) {
  return { name: error?.name ?? 'Error', code: error?.code ?? null, message: error?.message ?? String(error) };
}
function captureOptions(input: any) {
  return {
    projectRoot: input.projectRoot,
    agentModel: input.agentModel ?? null,
    hasProviderFactory: typeof input.providerFactory === 'function',
    hasCreateProvider: typeof input.createProvider === 'function',
    config: input.config
      ? { enabled: input.config.enabled, provider: input.config.provider }
      : null,
  };
}
function resourceShape(value: any) {
  return {
    keys: Object.keys(value).sort(),
    memory: value.memory !== undefined,
    maintenance: value.memoryService !== undefined,
    management: value.memoryManagement !== undefined,
  };
}
function bundleOptions(config: any, agentModel: any = null) {
  return {
    projectRoot: PROJECT,
    config: config ? { enabled: config.enabled, provider: config.provider } : null,
    agentModel,
  };
}
function stageResult(run: () => any) {
  try {
    return { result: resourceShape(run()) };
  } catch (error) {
    return { error: errorJson(error) };
  }
}

const bundleCases: any[] = [];

// 1. providerFactory is called with the full options, resources are returned,
//    and dispose is idempotent/asynchronous and calls the exact disposer once.
{
  const events: string[] = [];
  let captured: any;
  const memory = {};
  const maintenance = { async runDueScheduledMaintenance() { return {}; } };
  const management = { list() { return []; }, clear() {}, clearSession() {} };
  const bundle = new ProjectMemoryBundle({
    config: { enabled: true, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    modelConfig: { providers: {} },
    agentModel: 'test/model',
    projectRoot: PROJECT,
    providerFactory: (input: any) => {
      events.push('factory');
      captured = captureOptions(input);
      return {
        memory,
        maintenance,
        management,
        dispose: () => { events.push('dispose'); },
      };
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  steps.push({ op: 'stage-again', ...stageResult(() => bundle.stage()) });
  const first = bundle.dispose();
  events.push('after-dispose-call');
  const second = bundle.dispose();
  const beforeAwaitEvents = events.slice();
  await Promise.all([first, second]);
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents, afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'factory-success', factory: 'success', factoryCalls: 1, options: bundleOptions({ enabled: true, provider: 'edgeclaw' }, 'test/model'), factoryOptions: captured, steps, events });
}

// 2. A factory that throws consumes the stage attempt; dispose has no owner.
{
  const events: string[] = [];
  const bundle = new ProjectMemoryBundle({
    config: { enabled: true, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    projectRoot: PROJECT,
    providerFactory: () => {
      events.push('factory');
      throw new Error('factory boom');
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  steps.push({ op: 'stage-again', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['factory', 'after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'factory-throws', factory: 'throws', factoryCalls: 1, options: bundleOptions({ enabled: true, provider: 'edgeclaw' }), steps, events });
}

// 3. dispose() before stage() marks the bundle as disposing synchronously.
{
  const events: string[] = [];
  let factoryCalls = 0;
  const bundle = new ProjectMemoryBundle({
    config: undefined,
    projectRoot: PROJECT,
    providerFactory: () => {
      factoryCalls += 1;
      events.push('factory');
      return undefined;
    },
  });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  const steps: any[] = [{ op: 'dispose-call' }];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const beforeAwaitEvents = events.slice();
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose-await', beforeAwaitEvents, afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'dispose-before-stage', factory: 'undefined', options: bundleOptions(null), factoryCalls, steps, events });
}

// 4. An undefined factory result leaves the resources empty.
{
  const events: string[] = [];
  const bundle = new ProjectMemoryBundle({
    config: { enabled: false, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    projectRoot: PROJECT,
    providerFactory: () => {
      events.push('factory');
      return undefined;
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['factory', 'after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'factory-undefined', factory: 'undefined', factoryCalls: 1, options: bundleOptions({ enabled: false, provider: 'edgeclaw' }), steps, events });
}

// 5. No factory and no config: the real native factory early-returns
//    undefined without constructing EdgeClaw (no sqlite service is created).
{
  const events: string[] = [];
  const bundle = new ProjectMemoryBundle({
    config: undefined,
    projectRoot: PROJECT,
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'native-unconfigured', factory: 'none', factoryCalls: 0, options: bundleOptions(null), steps, events });
}

// 6. Deprecated createProvider(native) wraps provider/service and closes the
//    exact service once; its input has both factory fields stripped.
{
  const events: string[] = [];
  let captured: any;
  const memory = {};
  const service = {
    async runDueScheduledMaintenance() { return {}; },
    list() { return []; },
    clear() {},
    clearSession() {},
    close() { events.push('close'); },
  };
  const bundle = new ProjectMemoryBundle({
    config: { enabled: true, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    modelConfig: { providers: {} },
    agentModel: 'test/model',
    projectRoot: PROJECT,
    createProvider: (input: any) => {
      events.push('create');
      captured = captureOptions(input);
      return { provider: memory, service };
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['create', 'after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'native-create-provider', factory: 'native', factoryCalls: 1, options: bundleOptions({ enabled: true, provider: 'edgeclaw' }, 'test/model'), nativeOptions: captured, steps, events });
}

// 7. providerFactory wins when both injection points are present.
{
  const events: string[] = [];
  let captured: any;
  const memory = {};
  const bundle = new ProjectMemoryBundle({
    config: { enabled: true, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    projectRoot: PROJECT,
    providerFactory: (input: any) => {
      events.push('factory');
      captured = captureOptions(input);
      return { memory, dispose: () => { events.push('dispose'); } };
    },
    createProvider: () => {
      events.push('create');
      throw new Error('createProvider must not run');
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['factory', 'after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'provider-factory-preferred', factory: 'preferred', factoryCalls: 1, options: bundleOptions({ enabled: true, provider: 'edgeclaw' }), factoryOptions: captured, steps, events });
}

// 8. A provider without maintenance/management/dispose still gets all three
//    resource keys (the values are undefined); dispose is a no-op.
{
  const events: string[] = [];
  const memory = {};
  const bundle = new ProjectMemoryBundle({
    config: { enabled: true, provider: 'edgeclaw', captureStrategy: 'last_turn', includeAssistant: false },
    projectRoot: PROJECT,
    providerFactory: () => {
      events.push('factory');
      return { memory };
    },
  });
  const steps: any[] = [];
  steps.push({ op: 'stage', ...stageResult(() => bundle.stage()) });
  const disposal = bundle.dispose();
  events.push('after-dispose-call');
  await disposal;
  events.push('after-await');
  steps.push({ op: 'dispose', beforeAwaitEvents: ['factory', 'after-dispose-call'], afterAwaitEvents: events.slice() });
  bundleCases.push({ name: 'memory-only-provider', factory: 'memory-only', factoryCalls: 1, options: bundleOptions({ enabled: true, provider: 'edgeclaw' }), steps, events });
}

// The management methods are plain JS once extracted; execute the exact source
// bytes from createLocalGateway.ts with fake dialog/registry seams.
const createLocalGatewaySource = await readFile(join(baseline, 'src/cli/createLocalGateway.ts'), 'utf8');
const methodStart = createLocalGatewaySource.indexOf('    async memoryList(input) {');
const methodEnd = createLocalGatewaySource.indexOf('    async managerSessions(input) {', methodStart);
assert.ok(methodStart > 0, 'memoryList source was not found');
assert.ok(methodEnd > methodStart, 'memoryWipe source was not found');
const methodSource = createLocalGatewaySource.slice(methodStart, methodEnd);
assert.equal((methodSource.match(/async memory(List|Wipe)\(/g) ?? []).length, 2, 'expected exactly two methods');

const ITEMS = [
  { id: 'a', sourceSessionKey: 's1', title: 'one' },
  { id: 'b', title: 'no-session' },
  { id: 'c', sourceSessionKey: 's2' },
  { id: 'd', sourceSessionKey: 1 },
  { id: 'e', sourceSessionKey: 's1' },
  { id: 'f', sourceSessionKey: '' },
];
function cloneItems() {
  return JSON.parse(JSON.stringify(ITEMS));
}

const projectionCases: any[] = [];
async function projectionCase(
  name: string,
  options: { management: 'available' | 'unavailable'; items?: any[]; fail?: { op: string; message: string } },
  inputs: Array<{ op: 'list' | 'wipe'; input: any }>,
) {
  const items = options.items ?? cloneItems();
  const sharedCalls: any[] = [];
  const management = options.management === 'unavailable' ? undefined : {
    list(input: any) {
      sharedCalls.push({ op: 'list', limit: input?.limit });
      if (options.fail?.op === 'list') throw new Error(options.fail.message);
      return JSON.parse(JSON.stringify(items));
    },
    clear(scope: any) {
      sharedCalls.push({ op: 'clear', scope });
      if (options.fail?.op === 'clear') throw new Error(options.fail.message);
    },
    clearSession(sessionKey: any) {
      sharedCalls.push({ op: 'clearSession', sessionKey });
      if (options.fail?.op === 'clearSession') throw new Error(options.fail.message);
    },
  };
  const dialog = {
    projects: {
      resolveProjectKey: async (key: string) => {
        sharedCalls.push({ op: 'resolveProjectKey', key });
        return `resolved:${key}`;
      },
    },
  };
  const registry = {
    resolve: (key: string) => {
      sharedCalls.push({ op: 'resolve', key });
      return { memoryManagement: management };
    },
  };
  const boundApi = new Function(
    'dialog',
    'registry',
    'fallbackProjectRoot',
    'DialogGatewayError',
    `return { ${methodSource} };`,
  )(dialog, registry, FALLBACK, DialogGatewayError);
  assert.equal(typeof boundApi.memoryList, 'function');
  assert.equal(typeof boundApi.memoryWipe, 'function');

  const steps: any[] = [];
  for (const { op, input } of inputs) {
    const before = sharedCalls.length;
    let step: any = { op };
    try {
      const result = op === 'list'
        ? await boundApi.memoryList(input ?? {})
        : await boundApi.memoryWipe(input ?? {});
      step.resultJson = JSON.stringify(result);
    } catch (error) {
      step.error = errorJson(error);
    }
    step.input = input ?? {};
    step.calls = sharedCalls.slice(before);
    steps.push(step);
  }
  projectionCases.push({ name, management: options.management, items, ...(options.fail ? { fail: options.fail } : {}), steps });
}

await projectionCase('list-filter', { management: 'available' }, [
  { op: 'list', input: { projectKey: PROJECT } },
  { op: 'list', input: { projectKey: PROJECT, sessionKey: 's1' } },
  { op: 'list', input: { sessionKey: 's1' } },
  { op: 'list', input: { projectKey: PROJECT, sessionKey: '' } },
  { op: 'list', input: { projectKey: PROJECT, sessionKey: 'missing' } },
  { op: 'list', input: { projectKey: PROJECT, sessionKey: ' ' } },
]);
await projectionCase('wipe-branches', { management: 'available' }, [
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: 's1' } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: '  s1  ' } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: '   ' } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session' } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'project' } },
  { op: 'wipe', input: { projectKey: PROJECT } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'SESSION', sessionKey: 's1' } },
]);
await projectionCase('capability-unavailable', { management: 'unavailable' }, [
  { op: 'list', input: { projectKey: PROJECT } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: '   ' } },
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'project' } },
]);
await projectionCase('list-provider-throws', { management: 'available', fail: { op: 'list', message: 'list exploded' } }, [
  { op: 'list', input: { projectKey: PROJECT } },
]);
await projectionCase('clear-session-throws', { management: 'available', fail: { op: 'clearSession', message: 'clear session exploded' } }, [
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'session', sessionKey: 's1' } },
]);
await projectionCase('clear-project-throws', { management: 'available', fail: { op: 'clear', message: 'clear project exploded' } }, [
  { op: 'wipe', input: { projectKey: PROJECT, scope: 'project' } },
]);

const oracle = {
  sourceCommit,
  sourceSha256,
  bundleCases,
  projectionCases,
};
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(`wrote ${bundleCases.length} bundle cases and ${projectionCases.length} projection cases`);
