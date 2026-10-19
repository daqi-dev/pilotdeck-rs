// Records the fixed TypeScript baseline's SDK MCP configuration methods
// (src/cli/ProjectRuntimeRegistry.ts:1184-1308) with a fake gateway, fake
// project runtime and fake session-plugin source. Every method call is the
// real baseline implementation; only collaborators are injected.
//
// Run from the repo root with Node 22 and the baseline's installed tsx loader:
//   PDRS_BASELINE=/abs/path/PilotDeck \
//     node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/sdk-mcp/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/sdk-mcp
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const REGISTRY = 'src/cli/ProjectRuntimeRegistry.ts';
const RULE_REGISTRY = 'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts';

// Shared provenance guard (#280): explicit absolute Git, pinned HEAD, every
// tracked src byte equal to its Git blob, clean tracked tree.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'],
  { env: gitEnv }).toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
// The rule registry is exercised to observe the live ask array; both entries
// are part of the calibrating import closure.
const sourceSha256 = sourceClosureSha256(
  baseline, BASELINE_COMMIT, gitExecutable, [REGISTRY, RULE_REGISTRY],
);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const { ProjectRuntimeRegistry } = await import(
  pathToFileURL(join(baseline, REGISTRY)).href
);
const { GatewaySessionPermissionRuleSetRegistry } = await import(
  pathToFileURL(join(baseline, RULE_REGISTRY)).href
);

process.env.PDRS_MCP_SPACE = ' ';
delete process.env.PDRS_MCP_EMPTY;

type Step = Record<string, any> & { op: string };
type Scenario = {
  id: string;
  projectPlugins?: any[];
  sessionPlugins?: [string, any[]][];
  configured?: any;
  refreshError?: string;
  resolveError?: string;
  closeError?: string;
  gateway?: boolean;
  steps: Step[];
};

const PROJECT_ROOT = '/pdrs-fixture/project';
const stdio = (command = 'node') => ({ type: 'stdio', command, args: ['server.js'], env: { A: 'b' }, cwd: '/tmp', timeout: 5 });
const http = { type: 'streamable_http', url: 'http://x', headers: { H: 'v' }, timeout: 2 };

function clone(value: any) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

// Fixture input encoding for explicit `undefined` (JSON cannot carry it).
function decode(value: any): any {
  if (value && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === 1 && value.$ === 'undefined') return undefined;
  if (Array.isArray(value)) return value.map(decode);
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = decode(value[key]);
    return out;
  }
  return value;
}

function encodeState(value: any): any {
  if (value === undefined) return { $: 'undefined' };
  if (Array.isArray(value)) return value.map(encodeState);
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = encodeState(value[key]);
    return out;
  }
  return value;
}

function state(registry: any, lease: any) {
  return {
    servers: [...registry.sdkSessionMcpServers.entries()].map(([key, entries]: any) => [
      key,
      [...entries.entries()].map(([name, entry]: any) => [name, {
        config: encodeState(entry.config),
        enabled: encodeState(entry.enabled),
      }]),
    ]),
    overrides: [...registry.sdkMcpPermissionOverrides.entries()].map(([key, entries]: any) => [
      key, [...entries.entries()],
    ]),
    rules: [...registry.sdkMcpPermissionRules.entries()].map(([key, entries]: any) => [
      key, [...entries.entries()].map(([name, rule]: any) => [name, { ...rule }]),
    ]),
    projects: [...registry.sdkSessionProjects.entries()],
    ask: lease.rules.ask.map((rule: any) => ({ ...rule })),
    allow: lease.rules.allow.map((rule: any) => ({ ...rule })),
  };
}

function makeRegistry(scenario: Scenario) {
  const calls: any[] = [];
  const runtime = {
    projectRoot: PROJECT_ROOT,
    pluginRuntime: {
      refresh: async () => {
        calls.push(['refresh']);
        if (scenario.refreshError) throw new Error(scenario.refreshError);
      },
      snapshot: () => scenario.projectPlugins ?? [],
    },
  };
  const registry: any = Object.create(ProjectRuntimeRegistry.prototype);
  const permissionRuleSets = new GatewaySessionPermissionRuleSetRegistry();
  // The real registry stores the configured arrays by reference; clone so the
  // scenario's recorded `configured` input is not mutated by later steps.
  const lease = permissionRuleSets.acquire('s', clone(scenario.configured));
  permissionRuleSets.acquire('t', clone(scenario.configured));
  Object.assign(registry, {
    gateway: scenario.gateway === false ? undefined : {
      closeSession: async (input: any) => {
        calls.push(['closeSession', clone(input)]);
        if (scenario.closeError) throw new Error(scenario.closeError);
      },
    },
    permissionRuleSets,
    sdkSessionPlugins: new Map(scenario.sessionPlugins ?? []),
    sdkSessionMcpServers: new Map(),
    sdkMcpPermissionOverrides: new Map(),
    sdkMcpPermissionRules: new Map(),
    sdkSessionProjects: new Map(),
    sdkSessionPluginMap: scenario.sessionPlugins ?? [],
    resolve: (projectKey: any) => {
      calls.push(['resolve', projectKey === undefined ? null : projectKey]);
      if (scenario.resolveError) throw new Error(scenario.resolveError);
      return runtime;
    },
  });
  // The injected getter mirrors `this.sdkSessionPlugins.get(sessionKey) ?? []`.
  registry.fixtureSessionPlugins = (sessionKey: any) =>
    registry.sdkSessionPlugins.get(sessionKey) ?? [];
  return { registry, lease, calls };
}

async function run(registry: any, step: Step) {
  const input = decode(step.input ?? {});
  try {
    let result: any;
    switch (step.op) {
      case 'set':
        result = await registry.setMcpServersForSdk(input);
        break;
      case 'status':
        result = await registry.mcpServerStatusForSdk(input);
        break;
      case 'toggle':
        result = await registry.toggleMcpServerForSdk(input);
        break;
      case 'reconnect':
        result = await registry.reconnectMcpServerForSdk(input);
        break;
      case 'override':
        result = await registry.setMcpPermissionModeOverrideForSdk(input);
        break;
      case 'setSessionPlugins':
        // Fixture-only setup: the real `setSdkSessionConfig` owns this map in
        // A6.3-pr8; here it injects the session-plugin source for status.
        registry.sdkSessionPlugins.set(input.sessionKey, input.plugins);
        result = undefined;
        break;
      default:
        throw new Error(`unknown step ${step.op}`);
    }
    return result === undefined ? { ok: true } : { ok: result };
  } catch (error: any) {
    return { error: { name: error.name, code: error.code ?? null, message: error.message } };
  }
}

const scenarios: Scenario[] = [];
const add = (id: string, options: Omit<Scenario, 'id' | 'steps'>, steps: Step[]) => {
  scenarios.push({ id, ...options, steps });
};

const one = (overrides: Record<string, any> = {}) => ({ sessionKey: 's', servers: { a: stdio() }, ...overrides });

add('basic', {}, [
  { op: 'set', input: { sessionKey: 's', servers: { alpha: stdio(), beta: http } } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'alpha', enabled: false } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'alpha', enabled: true } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'alpha', enabled: false } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'reconnect', input: { sessionKey: 's', serverName: 'alpha' } },
  { op: 'reconnect', input: { sessionKey: 's', serverName: 'beta' } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'missing', enabled: true } },
  { op: 'reconnect', input: { sessionKey: 's', serverName: 'missing' } },
  { op: 'set', input: { sessionKey: 's', servers: { alpha: stdio() } } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'set', input: { sessionKey: 's', servers: { alpha: stdio('other') } } },
  { op: 'set', input: { sessionKey: 's', servers: {} } },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('invalid-configs', {}, [
  { op: 'set', input: { sessionKey: 's' } },
  { op: 'set', input: { sessionKey: 's', servers: null } },
  { op: 'set', input: { sessionKey: 's', servers: { a: {} } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: '' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'streamable_http' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'streamable_http', url: '' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: null } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: 'not-an-object' } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: 5 } } },
  { op: 'set', input: { sessionKey: 's', servers: [stdio()] } },
  { op: 'set', input: { sessionKey: 's', servers: 'ab' } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: 'node', args: 5 } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: 'node', args: 'xy' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'streamable_http', httpUrl: 'http://x' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'streamable_http', url: '${env:PDRS_MCP_EMPTY}' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: '${env:PDRS_MCP_SPACE}' } } } },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('reserved-names', {
  projectPlugins: [{ name: 'project', mcpServers: { proj: { command: 'x' } } }],
  sessionPlugins: [['s', [{ name: 'session', mcpServers: { sess: { command: 'y' } } }]]],
}, [
  { op: 'set', input: { sessionKey: 's', servers: { proj: stdio(), sess: http, ok: stdio() } } },
  { op: 'set', input: { sessionKey: 's', servers: { ok: stdio() } } },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('permission', {
  configured: { ask: [{ source: 'policy', behavior: 'ask', toolName: 'Bash' }] },
}, [
  { op: 'override', input: { sessionKey: 's', serverName: 'my server', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'my server', mode: 'default' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'my server', mode: null } },
  { op: 'override', input: { sessionKey: 's', serverName: 'my server', mode: null } },
  { op: 'override', input: { sessionKey: 's', serverName: 'a b', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'a__b', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: '_a_', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: '😀', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'a😀b', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: '---', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'plain', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'plain', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'plain', mode: null } },
  { op: 'override', input: { serverName: 'x', mode: 'auto' } },
  { op: 'override', input: { sessionKey: '', serverName: 'x', mode: 'auto' } },
  { op: 'override', input: { sessionKey: null, serverName: 'x', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: '', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: null, mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x', mode: 'bogus' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x', mode: 3 } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x', mode: { a: 1 } } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x', mode: ['a', 'b'] } },
  { op: 'override', input: { sessionKey: 'other', serverName: 'no-live-entry', mode: 'auto' } },
]);

add('permission-session-order', {
  configured: { ask: [{ source: 'policy', behavior: 'ask', toolName: 'Bash' }] },
}, [
  { op: 'override', input: { sessionKey: 's', serverName: 'server-s1', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 't', serverName: 'server-t1', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'server-s2', mode: 'auto' } },
]);

add('status-order', {
  projectPlugins: [{ name: 'project', mcpServers: { proj: { command: 'x' } } }],
}, [
  { op: 'set', input: { sessionKey: 's', servers: { b: http, c: stdio() } } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'b', enabled: false } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'setSessionPlugins', input: { sessionKey: 's', plugins: [{ name: 'session', mcpServers: { a: { command: 'z' }, b: { command: 'y' } } }] } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'status', input: {} },
  { op: 'status', input: { sessionKey: '' } },
  { op: 'status', input: { sessionKey: null } },
]);

add('explicit-undefined', {
  configured: { ask: [{ source: 'policy', behavior: 'ask', toolName: 'Bash' }] },
}, [
  { op: 'override', input: { sessionKey: { $: 'undefined' }, serverName: 'x', mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: { $: 'undefined' }, mode: 'auto' } },
  { op: 'override', input: { sessionKey: 's', serverName: 'x', mode: { $: 'undefined' } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { $: 'undefined' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: 'node' } } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: 'node', args: { $: 'undefined' } } } } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: { $: 'undefined' } } },
  { op: 'set', input: { sessionKey: 's', servers: { a: { type: 'stdio', command: 'node', args: { $: 'undefined' } } } } },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('errors', { refreshError: 'refresh boom' }, [
  { op: 'set', input: one() },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('resolve-error', { resolveError: 'resolve boom' }, [
  { op: 'set', input: one() },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('close-error', { closeError: 'close boom' }, [
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: false } },
]);

add('no-gateway', { gateway: false }, [
  { op: 'set', input: one() },
  { op: 'reconnect', input: { sessionKey: 's', serverName: 'a' } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: false } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: true } },
  { op: 'status', input: { sessionKey: 's' } },
]);

add('config-key-order', {}, [
  {
    op: 'set',
    input: {
      sessionKey: 's',
      servers: { a: { type: 'stdio', command: 'node', z: 1, 2: 'two', 1: 'one', a: 2 } },
    },
  },
  {
    op: 'set',
    input: {
      sessionKey: 's',
      servers: { a: { type: 'stdio', command: 'node', z: 1, 2: 'two', 1: 'one', a: 2 } },
    },
  },
  {
    op: 'set',
    input: {
      sessionKey: 's',
      servers: { a: { type: 'stdio', command: 'changed', z: 1, 2: 'two', 1: 'one', a: 2 } },
    },
  },
]);

add('toggle-enabled', {}, [
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: null } },
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: 0 } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: '' } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a', enabled: 'false' } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'set', input: one() },
  { op: 'toggle', input: { sessionKey: 's', serverName: 'a' } },
  { op: 'status', input: { sessionKey: 's' } },
  { op: 'set', input: one() },
  { op: 'status', input: { sessionKey: 's' } },
]);

const oracleScenarios: any[] = [];
let stepCount = 0;
for (const scenario of scenarios) {
  const { registry, lease, calls } = makeRegistry(scenario);
  const steps: any[] = [];
  for (const step of scenario.steps) {
    const result = await run(registry, step);
    const projected = state(registry, lease);
    steps.push({ step, result, state: projected, calls: clone(calls) });
    stepCount += 1;
  }
  oracleScenarios.push({
    id: scenario.id,
    projectPlugins: scenario.projectPlugins ?? [],
    sessionPlugins: scenario.sessionPlugins ?? [],
    configured: scenario.configured ?? null,
    refreshError: scenario.refreshError ?? null,
    resolveError: scenario.resolveError ?? null,
    closeError: scenario.closeError ?? null,
    gateway: scenario.gateway !== false,
    steps,
  });
  console.log(`${scenario.id}: ${scenario.steps.length} steps, calls=${calls.length}`);
}

// Self-checks: prove the recorded steps really hit the branches this fixture
// is meant to calibrate instead of silently comparing empty outcomes.
const scenario = (id: string) => oracleScenarios.find((entry) => entry.id === id);
assert.deepEqual(scenario('basic')!.steps[0].result,
  { ok: { added: ['alpha', 'beta'], removed: [], errors: [] } });
assert.equal(scenario('basic')!.steps[5].result.ok.servers[0].status, 'disabled');
assert.equal(scenario('basic')!.steps[6].result.error.code, 'MCP_SERVER_DISABLED');
assert.equal(scenario('basic')!.steps[8].result.error.code, 'MCP_SERVER_NOT_FOUND');
assert.deepEqual(scenario('basic')!.steps[10].result.ok, { added: [], removed: ['beta'], errors: [] });
assert.deepEqual(scenario('basic')!.steps[12].result.ok, { added: ['alpha'], removed: [], errors: [] });
assert.deepEqual(scenario('basic')!.steps[13].result.ok, { added: [], removed: ['alpha'], errors: [] });
assert.equal(scenario('invalid-configs')!.steps[7].result.error.name, 'TypeError');
assert.equal(scenario('invalid-configs')!.steps[9].result.ok.errors[0].error,
  'no recognized transport (need command or url)');
assert.equal(scenario('invalid-configs')!.steps[10].result.ok.added[0], '0');
assert.equal(scenario('invalid-configs')!.steps[12].result.error.message, 'config.args is not iterable');
assert.equal(scenario('invalid-configs')!.steps[13].result.ok.added[0], 'a');
assert.equal(scenario('invalid-configs')!.steps[15].result.ok.errors[0].error,
  'no recognized transport (need command or url)');
assert.equal(scenario('reserved-names')!.steps[0].result.ok.errors.length, 2);
assert.equal(scenario('reserved-names')!.steps[2].result.ok.servers.length, 2);
assert.equal(scenario('permission')!.steps[0].result.ok.warning,
  'PilotDeck does not expose Claude\'s MCP safety classifier; auto uses conservative ask semantics for this server.');
assert.equal(scenario('permission')!.steps[0].state.ask.length, 2);
assert.equal(scenario('permission')!.steps[2].state.ask.length, 1);
assert.equal(scenario('permission')!.steps[11].state.ask.length, 8);
assert.equal(scenario('permission')!.steps[11].state.rules[0][1].length, 7);
assert.equal(scenario('permission')!.steps[13].result.error.code, 'INVALID_SESSION_KEY');
assert.equal(scenario('permission')!.steps[19].result.error.message,
  'Unsupported MCP permission mode: undefined');
assert.equal(scenario('permission')!.steps[20].result.error.message,
  'Unsupported MCP permission mode: bogus');
assert.equal(scenario('permission')!.steps[22].result.error.message,
  'Unsupported MCP permission mode: [object Object]');
assert.equal(scenario('permission')!.steps[23].result.error.message,
  'Unsupported MCP permission mode: a,b');
assert.equal(scenario('permission')!.steps[24].state.rules.some(([key]: any) => key === 'other'), false);
assert.equal(scenario('permission')!.steps[24].state.overrides.some(([key]: any) => key === 'other'), true);
assert.deepEqual(scenario('status-order')!.steps[2].result.ok.servers, [{ name: 'b', status: 'disabled' }, { name: 'c', status: 'configured' }]);
assert.deepEqual(scenario('status-order')!.steps[4].result.ok.servers, [{ name: 'a', status: 'configured' }, { name: 'b', status: 'disabled' }, { name: 'c', status: 'configured' }]);
assert.equal(scenario('status-order')!.steps[5].result.ok.servers.length, 0);
assert.equal(scenario('errors')!.steps[0].result.error.message, 'refresh boom');
assert.equal(scenario('resolve-error')!.steps[0].result.error.message, 'resolve boom');
assert.equal(scenario('close-error')!.steps[0].result.error.message, 'close boom');
assert.equal(scenario('close-error')!.steps[0].state.servers.length, 1);
assert.equal(scenario('no-gateway')!.steps[1].result.ok, true);
assert.equal(scenario('no-gateway')!.steps[3].result.ok.servers[0].status, 'disabled');
assert.equal(scenario('no-gateway')!.steps[5].result.ok.servers[0].status, 'configured');
assert.equal(scenario('no-gateway')!.steps.every((entry: any) => entry.calls.every((call: any) => call[0] !== 'closeSession')), true);
assert.equal(scenario('toggle-enabled')!.steps[1].state.servers[0][1][0][1].enabled, null);
assert.equal(scenario('toggle-enabled')!.steps[4].result.ok.servers[0].status, 'disabled');
assert.equal(scenario('toggle-enabled')!.steps[10].result.ok.servers[0].status, 'configured');
assert.deepEqual(scenario('permission-session-order')!.steps[1].state.rules.map(([key]: any) => key), ['s', 't']);
assert.deepEqual(scenario('permission-session-order')!.steps[2].state.rules.map(([key]: any) => key), ['s', 't']);
assert.equal(scenario('permission-session-order')!.steps[2].state.rules[0][1].length, 2);
assert.equal(scenario('permission-session-order')!.steps[2].state.ask.length, 3);
assert.equal(scenario('explicit-undefined')!.steps[0].result.error.code, 'INVALID_SESSION_KEY');
assert.equal(scenario('explicit-undefined')!.steps[1].result.error.code, 'INVALID_MCP_SERVER');
assert.equal(scenario('explicit-undefined')!.steps[2].result.error.message,
  'Unsupported MCP permission mode: undefined');
assert.equal(scenario('explicit-undefined')!.steps[3].result.error.name, 'TypeError');
assert.equal(scenario('explicit-undefined')!.steps[3].result.error.message,
  "Cannot read properties of undefined (reading 'type')");
assert.deepEqual(scenario('explicit-undefined')!.steps[5].result.ok, { added: [], removed: [], errors: [] });
assert.equal(scenario('explicit-undefined')!.steps[5].state.servers[0][1][0][1].config.args.$, 'undefined');
assert.equal(scenario('explicit-undefined')!.steps[6].state.servers[0][1][0][1].enabled.$, 'undefined');
assert.deepEqual(scenario('explicit-undefined')!.steps[7].result.ok, { added: ['a'], removed: [], errors: [] });
assert.equal(scenario('explicit-undefined')!.steps[8].result.ok.servers[0].status, 'configured');
assert.equal(stepCount, oracleScenarios.reduce((sum, entry) => sum + entry.steps.length, 0));

const oracle = { sourceCommit, sourceSha256, scenarios: oracleScenarios };
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(`PASS: ${oracleScenarios.length} scenarios, ${stepCount} steps, ${Object.keys(sourceSha256).length} source files`);
