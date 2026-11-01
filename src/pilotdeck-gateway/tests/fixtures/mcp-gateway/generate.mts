// Real TS golden for the U6 MCP control gateway adapter (#1215). The four
// control methods run through the real three-layer stack: real SessionRouter
// (real beginTurn/close, real AgentHandle sessions), real
// GatewaySessionPermissionRuleSetRegistry, real InProcessGateway (only the
// interactionCoordinator is wrapped on the prototype chain to record
// close-session:<reason>), a real ProjectRuntimeRegistry prototype (only
// resolve/pluginRuntime are stubbed, as in sdk-mcp-p2), and a real
// GatewayWsConnection over a fake text transport exchanging real frames.
// Each case = one request frame, recording the WS response, the event stream
// (resolve/refresh/snapshot/create/dispose/evict/close-session) and the post
// state (active turns, live sessions, MCP tables, ask rules).
//
// Run from the repo root with Node 22 and the fixed baseline's tsx loader:
//   PDRS_BASELINE=/path/to/PilotDeck node --import .../tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/mcp-gateway/generate.mts <scratch>
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.equal(process.version, 'v22.22.0', '#1215 exact Node baseline');
assert.ok(process.env.PDRS_BASELINE, '#1215 set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], '#1215 provide the fixture output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/server/GatewayWsConnection.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/client/GatewayInteractionCoordinator.ts',
  'src/gateway/SessionRouter.ts',
  'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/agent/scope/AgentHandle.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);

await mkdir(join(output, 'home'), { recursive: true });
process.env.HOME = join(output, 'home');

const { SessionRouter } = await import(pathToFileURL(join(baseline, 'src/gateway/SessionRouter.ts')).href);
const { InProcessGateway } = await import(pathToFileURL(join(baseline, 'src/gateway/client/InProcessGateway.ts')).href);
const { GatewayWsConnection } = await import(pathToFileURL(join(baseline, 'src/gateway/server/GatewayWsConnection.ts')).href);
const { PILOTDECK_GATEWAY_PROTOCOL_VERSION } = await import(pathToFileURL(join(baseline, 'src/gateway/protocol/version.ts')).href);
const { ProjectRuntimeRegistry } = await import(pathToFileURL(join(baseline, 'src/cli/ProjectRuntimeRegistry.ts')).href);
const { GatewaySessionPermissionRuleSetRegistry } = await import(pathToFileURL(join(baseline, 'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts')).href);
const { GatewayInteractionCoordinator } = await import(pathToFileURL(join(baseline, 'src/gateway/client/GatewayInteractionCoordinator.ts')).href);
const { asAgentHandle } = await import(pathToFileURL(join(baseline, 'src/agent/scope/AgentHandle.ts')).href);

const events: string[] = [];
// The interaction coordinator is REPLACED on the prototype chain: the record
// replaces `closeSession`, so the real `GatewayInteractionCoordinator`
// closeSession side effects never run. Only the reason strings the four
// methods pass are pinned here; the coordinator layer itself is outside this
// component's parity scope (P3-2).
const realCoordinator = new GatewayInteractionCoordinator({});
const coordinator = Object.create(realCoordinator);
coordinator.closeSession = (sessionKey: string, reason: string) => {
  events.push(`close-session:${sessionKey}:${reason}`);
};

// Real AgentHandle sessions; key `f` disposes with a real rejection, so the
// close-fault cells fail through the real asAgentHandle AggregateError path
// (evict still fires from beginEviction's finally).
const session = (key: string) => ({
  sessionId: `sess-${key}`,
  snapshot: () => ({ sessionId: `sess-${key}`, messages: [] }),
  pendingTurns: () => [],
  pendingTurnCount: 0,
  discardQueuedTurns: async () => {},
  abort: () => {},
  dispose: async () => {
    events.push(`dispose:${key}`);
    if (key === 'f') throw new Error('fixture dispose failure');
  },
});
const router = new SessionRouter({
  createSession: async (context: any) => {
    events.push(`create:${context.sessionKey}`);
    return asAgentHandle(session(context.sessionKey));
  },
  onSessionEvict: (sessionKey: string, reason: string) => {
    events.push(`evict:${sessionKey}:${reason}`);
  },
  idleSweepIntervalMs: 0,
  idleSessionTimeoutMs: 60 * 60 * 1000,
});

const permissionRuleSets = new GatewaySessionPermissionRuleSetRegistry();
// One lease per scenario key, acquired up front and read live in post state.
const KEYS = ['s', ' w ', 'b', 'd', 'o', 'f', 't', 'tb', 'rb', 'v', 'vb'];
const leases = new Map(KEYS.map((key) => [key, permissionRuleSets.acquire(key, [])] as const));
let turnOnNextRefresh: { key: string; run: string } | undefined;
const runtime = {
  projectRoot: '/pdrs-fixture/project',
  pluginRuntime: {
    refresh: async () => {
      events.push('refresh');
      if (turnOnNextRefresh) {
        const { key, run } = turnOnNextRefresh;
        turnOnNextRefresh = undefined;
        router.beginTurn(key, run);
      }
    },
    snapshot: () => {
      events.push('snapshot');
      return [];
    },
  },
};
const registry: any = Object.create(ProjectRuntimeRegistry.prototype);
Object.assign(registry, {
  permissionRuleSets,
  sdkSessionPlugins: new Map(),
  sdkSessionMcpServers: new Map(),
  sdkMcpPermissionOverrides: new Map(),
  sdkMcpPermissionRules: new Map(),
  sdkSessionProjects: new Map(),
  resolve: (projectKey: any) => {
    events.push(`resolve:${projectKey ?? 'null'}`);
    return runtime;
  },
});

const gateway = new InProcessGateway(router, {
  interactionCoordinator: coordinator as any,
  setMcpServers: (input: any) => registry.setMcpServersForSdk(input),
  reconnectMcpServer: (input: any) => registry.reconnectMcpServerForSdk(input),
  toggleMcpServer: (input: any) => registry.toggleMcpServerForSdk(input),
  setMcpPermissionModeOverride: (input: any) => registry.setMcpPermissionModeOverrideForSdk(input),
});
registry.setGateway(gateway);

// --- WS layer: a fake text connection exchanging real frames ----------------
const waiters: ((message: string) => void)[] = [];
let notify: ((message: string) => void) | undefined;
const connect = (connectionGateway: any) => {
  const ws = {
    onMessage(handler: (message: string) => void) { notify = handler; },
    onClose(_handler: () => void) {},
    sendText(message: string) {
      for (const waiter of waiters.splice(0)) waiter(message);
    },
    close() {},
  };
  new GatewayWsConnection(ws as any, { gateway: connectionGateway, token: 'probe-token', serverVersion: 'probe' });
  // Register the waiter before notify(): a request whose dispatch throws
  // synchronously (the unknown-method default case) answers inside the
  // notify() call itself.
  const hello = new Promise<string>((done) => waiters.push(done));
  notify!(JSON.stringify({
    type: 'hello', protocolVersion: PILOTDECK_GATEWAY_PROTOCOL_VERSION,
    clientName: 'probe', clientVersion: '0', token: 'probe-token',
  }));
  return hello;
};
await connect(gateway as any);

let next = 0;
async function rawRequest(method: string, params: unknown): Promise<Record<string, unknown>> {
  const id = `r${next++}`;
  const frame: Record<string, unknown> = { type: 'request', id, method };
  if (params !== undefined) frame.params = params;
  events.length = 0;
  const received = new Promise<string>((done) => waiters.push(done));
  notify!(JSON.stringify(frame));
  const response: Record<string, unknown> = JSON.parse(await received);
  delete response.id;
  return response;
}

function postState() {
  const post: {
    turns: string[];
    sessions: Record<string, boolean>;
    servers: Record<string, Array<[string, unknown]>>;
    ask: Record<string, unknown[]>;
  } = { turns: [], sessions: {}, servers: {}, ask: {} };
  post.turns = KEYS.filter((key) => router.hasActiveTurn(key));
  for (const key of KEYS) {
    post.sessions[key] = router.hasSession(key);
    post.servers[key] = [...(registry.sdkSessionMcpServers.get(key) ?? new Map()).entries()]
      .map(([name, entry]) => [name, entry.enabled]);
    post.ask[key] = (leases.get(key) as any)?.rules?.ask?.slice() ?? [];
  }
  return post;
}

async function runSetup(step: string) {
  const [action, key, arg] = step.split(':');
  assert.ok(KEYS.includes(key), `setup key ${key} must be a scenario key`);
  if (action === 'ensure-session') {
    await router.getOrCreate({ sessionKey: key, projectKey: '/p' } as any);
  } else if (action === 'begin-turn') {
    router.beginTurn(key, arg);
  } else if (action === 'seed-set') {
    await gateway.setMcpServers({ sessionKey: key, servers: { [arg]: { type: 'stdio', command: 'node' } } });
  } else if (action === 'session-plugin') {
    registry.sdkSessionPlugins.set(key, [{ mcpServers: { [arg]: { type: 'stdio', command: 'node' } } }]);
  } else if (action === 'refresh-begins-turn') {
    turnOnNextRefresh = { key, run: arg };
  } else {
    assert.fail(`unknown setup action ${action}`);
  }
}

const rows: any[] = [];
async function run(id: string, method: string, params: unknown, setup: string[] = []) {
  for (const step of setup) await runSetup(step);
  const response = await rawRequest(method, params);
  // `undefined` params must stay distinct from `params: null`: the frame is
  // sent without the key, and TS answers "...of undefined..." vs "...of
  // null...". Recording `params ?? null` would collapse the two frames into
  // one golden row (fix-r1 P2-2), so the key is simply omitted.
  rows.push({ id, method, ...(params === undefined ? {} : { params }), setup, events: [...events], response, post: postState() });
}

// --- set_mcp_servers ---------------------------------------------------------
await run('set-success', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node', args: ['server.js'] },
  beta: { type: 'streamable_http', url: 'http://x' },
} }, ['ensure-session:s']);
await run('set-change', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node', args: ['server.js'] },
  gamma: { type: 'stdio', command: 'node' },
} }, ['ensure-session:s']);
await run('set-invalid-config', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node', args: ['server.js'] },
  bad: { type: 'stdio' },
} }, ['ensure-session:s']);
await run('set-reserved-name', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node', args: ['server.js'] },
  pluginOwned: { type: 'stdio', command: 'node' },
} }, ['ensure-session:s', 'session-plugin:s:pluginOwned']);
await run('set-blank-session-key', 'set_mcp_servers', { sessionKey: '   ', servers: {} });
await run('set-null-params', 'set_mcp_servers', null);
await run('set-params-omitted', 'set_mcp_servers', undefined);
await run('set-nonstring-session-key', 'set_mcp_servers', { sessionKey: 5, servers: {} });
await run('set-missing-session-key', 'set_mcp_servers', { servers: {} });
await run('set-missing-servers', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p' }, ['ensure-session:s']);
await run('set-whitespace-key', 'set_mcp_servers', { sessionKey: ' w ', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node' },
} }, ['ensure-session: w ']);
await run('set-busy', 'set_mcp_servers', { sessionKey: 'b', servers: {
  alpha: { type: 'stdio', command: 'node' },
} }, ['ensure-session:b', 'begin-turn:b:run-1']);
await run('set-other-key-busy', 'set_mcp_servers', { sessionKey: 's', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node' },
} }, ['ensure-session:o', 'begin-turn:o:run-2', 'ensure-session:s']);
await run('set-turn-during-callback', 'set_mcp_servers', { sessionKey: 'd', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node' },
} }, ['ensure-session:d', 'refresh-begins-turn:d:run-3']);
await run('set-close-fault', 'set_mcp_servers', { sessionKey: 'f', projectKey: '/p', servers: {
  alpha: { type: 'stdio', command: 'node' },
} }, ['ensure-session:f']);

// --- mcp_server_toggle / mcp_server_reconnect --------------------------------
await run('toggle-off', 'mcp_server_toggle', { sessionKey: 't', serverName: 'alpha', enabled: false },
  ['seed-set:t:alpha', 'ensure-session:t']);
await run('reconnect-disabled', 'mcp_server_reconnect', { sessionKey: 't', serverName: 'alpha' });
await run('toggle-on', 'mcp_server_toggle', { sessionKey: 't', serverName: 'alpha', enabled: true },
  ['ensure-session:t']);
await run('reconnect-success', 'mcp_server_reconnect', { sessionKey: 't', serverName: 'alpha' },
  ['ensure-session:t']);
await run('reconnect-unknown', 'mcp_server_reconnect', { sessionKey: 't', serverName: 'missing' });
await run('toggle-unknown', 'mcp_server_toggle', { sessionKey: 't', serverName: 'missing', enabled: false });
await run('toggle-missing-server-name', 'mcp_server_toggle', { sessionKey: 't', enabled: false });
await run('toggle-blank-server-name', 'mcp_server_toggle', { sessionKey: 't', serverName: '  ', enabled: false });
await run('toggle-null-params', 'mcp_server_toggle', null);
await run('toggle-busy', 'mcp_server_toggle', { sessionKey: 'tb', serverName: 'alpha', enabled: false },
  ['begin-turn:tb:run-4']);
await run('reconnect-busy', 'mcp_server_reconnect', { sessionKey: 'rb', serverName: 'alpha' },
  ['begin-turn:rb:run-5']);
await run('reconnect-missing-fields', 'mcp_server_reconnect', { sessionKey: 't' });
await run('toggle-missing-session-key', 'mcp_server_toggle', { serverName: 'alpha', enabled: false });
await run('toggle-blank-session-key', 'mcp_server_toggle', { sessionKey: '  ', serverName: 'alpha', enabled: false });
await run('reconnect-missing-session-key', 'mcp_server_reconnect', { serverName: 'alpha' });

// --- set_mcp_permission_mode_override -----------------------------------------
await run('override-auto', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: 'auto' },
  ['seed-set:v:alpha', 'ensure-session:v']);
await run('override-default', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: 'default' },
  ['ensure-session:v']);
await run('override-auto-again', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: 'auto' },
  ['ensure-session:v']);
await run('override-null', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: null },
  ['ensure-session:v']);
await run('override-invalid-mode', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: 'fast' });
await run('override-missing-mode', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha' });
await run('override-number-mode', 'set_mcp_permission_mode_override', { sessionKey: 'v', serverName: 'alpha', mode: 42 });
await run('override-busy-valid', 'set_mcp_permission_mode_override', { sessionKey: 'vb', serverName: 'alpha', mode: 'auto' },
  ['begin-turn:vb:run-6']);
await run('override-busy-invalid-mode', 'set_mcp_permission_mode_override', { sessionKey: 'vb', serverName: 'alpha', mode: 'fast' });
await run('override-missing-fields', 'set_mcp_permission_mode_override', { sessionKey: 'vb' });
await run('override-missing-session-key', 'set_mcp_permission_mode_override', { serverName: 'alpha', mode: 'auto' });
await run('override-nonstring-session-key', 'set_mcp_permission_mode_override', { sessionKey: 5, serverName: 'alpha', mode: 'auto' });
await run('override-null-params', 'set_mcp_permission_mode_override', null);
await run('override-close-fault', 'set_mcp_permission_mode_override', { sessionKey: 'f', serverName: 'alpha', mode: 'auto' },
  ['ensure-session:f']);

// --- assembly-layer cases (recorded, not component-replayable) ----------------
// A gateway assembled without the four options answers CAPABILITY_UNAVAILABLE
// per method; the Rust component is constructed with the capability, so these
// document the wiring-layer contract for the future routing PR.
const bare = new InProcessGateway(new SessionRouter({
  createSession: async () => asAgentHandle(session('bare')),
  idleSweepIntervalMs: 0,
}), {});
await connect(bare as any);
const capabilityCases: any[] = [];
for (const method of ['set_mcp_servers', 'mcp_server_reconnect', 'mcp_server_toggle', 'set_mcp_permission_mode_override']) {
  const response = await rawRequest(method, { sessionKey: 's' });
  capabilityCases.push({ method, response });
}
await connect(gateway as any);
const unknownMethod = await rawRequest('mcp_server_pause', { sessionKey: 's' });

// --- coverage guards: pin the matrix expectations the Rust replay asserts ---
const byId = (id: string) => rows.find((row) => row.id === id);
assert.equal(byId('set-success').response.ok, true, 'set-success ok');
assert.deepEqual(byId('set-success').response.result.added, ['alpha', 'beta'], 'set-success added');
assert.deepEqual(byId('set-success').events,
  ['resolve:/p', 'refresh', 'snapshot', 'dispose:s', 'evict:s:closed', 'close-session:s:sdk_mcp_servers_changed'],
  'set-success closes the session through both layers');
assert.equal(byId('set-success').post.sessions.s, false, 'set-success evicts');
assert.deepEqual(byId('set-change').response.result.added, ['gamma'], 'set-change added');
assert.deepEqual(byId('set-change').response.result.removed, ['beta'], 'set-change removed');
assert.equal(byId('set-invalid-config').response.ok, true, 'set-invalid-config still succeeds');
assert.equal(byId('set-invalid-config').response.result.errors.length, 1, 'set-invalid-config reports the entry');
assert.deepEqual(byId('set-invalid-config').events,
  ['resolve:/p', 'refresh', 'snapshot', 'dispose:s', 'evict:s:closed'],
  'set-invalid-config evicts without the registry closeSession');
assert.equal(byId('set-reserved-name').response.result.errors[0].error,
  'This MCP server name is owned by project configuration or a plugin.', 'reserved-name error text');
assert.equal(byId('set-blank-session-key').response.error.code, 'INVALID_SESSION_KEY', 'blank key code');
assert.equal(byId('set-blank-session-key').response.error.message, 'sessionKey is required.', 'blank key message');
assert.deepEqual(byId('set-blank-session-key').events, [], 'blank key never resolves');
assert.equal(byId('set-null-params').response.error.code, 'gateway_request_failed', 'null params envelope');
assert.equal(byId('set-null-params').response.error.message,
  'Cannot read properties of null (reading \'sessionKey\')', 'null params message');
assert.equal(byId('set-params-omitted').response.error.code, 'gateway_request_failed', 'omitted params envelope');
assert.equal(byId('set-params-omitted').response.error.message,
  'Cannot read properties of undefined (reading \'sessionKey\')', 'omitted params message');
assert.equal(JSON.stringify(byId('set-params-omitted').params), undefined, 'omitted params stays absent');
assert.equal(byId('set-nonstring-session-key').response.error.message,
  'input.sessionKey?.trim is not a function', 'non-string key message');
assert.equal(byId('set-missing-session-key').response.error.code, 'INVALID_SESSION_KEY', 'missing key code');
assert.equal(byId('set-missing-session-key').response.error.message, 'sessionKey is required.', 'missing key message');
assert.deepEqual(byId('set-missing-session-key').events, [], 'missing key never resolves');
assert.deepEqual(byId('set-missing-servers').response.result.removed, ['alpha', 'gamma'], 'missing servers removes all');
assert.equal(registry.sdkSessionMcpServers.has('w'), false, 'whitespace key is not normalized');
assert.ok(registry.sdkSessionMcpServers.has(' w '), 'whitespace key stores servers under the raw key');
assert.deepEqual(byId('set-whitespace-key').post.servers[' w '], [['alpha', true]],
  'whitespace key stores servers under the raw key');
assert.equal(byId('set-whitespace-key').post.sessions[' w '], false, 'whitespace key session closed');
assert.equal(byId('set-busy').response.error.code, 'SESSION_BUSY', 'set busy code');
assert.equal(byId('set-busy').response.error.message, 'Cannot change MCP servers while a turn is active.', 'set busy message');
assert.deepEqual(byId('set-busy').events, [], 'set busy runs no callback');
assert.deepEqual(byId('set-busy').post.turns, ['b'], 'set busy keeps the turn');
assert.equal(byId('set-other-key-busy').response.ok, true, 'busy gate is per key');
assert.deepEqual(byId('set-other-key-busy').post.turns, ['b', 'o'], 'other-key turn survives');
assert.equal(byId('set-turn-during-callback').response.ok, true, 'turn during callback succeeds');
assert.deepEqual(byId('set-turn-during-callback').post.turns, ['b', 'o'], 'close clears the in-flight turn');
assert.equal(byId('set-close-fault').response.error.code, 'gateway_request_failed', 'close fault envelope');
assert.equal(byId('set-close-fault').response.error.message, 'Failed to dispose agent handle.', 'close fault message');
assert.deepEqual(byId('set-close-fault').events,
  ['resolve:/p', 'refresh', 'snapshot', 'dispose:f', 'evict:f:closed'], 'close fault still evicts');
assert.deepEqual(byId('set-close-fault').post.servers.f, [['alpha', true]], 'close fault keeps the commit');
assert.deepEqual(byId('toggle-off').response.result, { ok: true }, 'toggle result shape');
assert.deepEqual(byId('toggle-off').events,
  ['dispose:t', 'evict:t:closed', 'close-session:t:sdk_mcp_server_toggled'], 'toggle closes');
assert.deepEqual(byId('toggle-off').post.servers.t, [['alpha', false]], 'toggle persists enabled');
assert.equal(byId('reconnect-disabled').response.error.code, 'MCP_SERVER_DISABLED', 'disabled code');
assert.equal(byId('reconnect-disabled').response.error.message,
  'SDK-owned MCP server alpha is disabled; enable it before reconnecting.', 'disabled message');
assert.deepEqual(byId('reconnect-disabled').events, [], 'disabled never closes');
assert.deepEqual(byId('reconnect-success').response.result, { ok: true }, 'reconnect result shape');
assert.deepEqual(byId('reconnect-success').events,
  ['dispose:t', 'evict:t:closed', 'close-session:t:sdk_mcp_server_reconnect'], 'reconnect closes');
assert.equal(byId('reconnect-unknown').response.error.code, 'MCP_SERVER_NOT_FOUND', 'reconnect unknown code');
assert.equal(byId('reconnect-unknown').response.error.message,
  'SDK-owned MCP server missing is not configured for session t.', 'reconnect unknown message');
assert.deepEqual(byId('reconnect-unknown').events, [], 'reconnect unknown never closes');
assert.equal(byId('toggle-unknown').response.error.code, 'MCP_SERVER_NOT_FOUND', 'toggle unknown code');
assert.equal(byId('toggle-missing-server-name').response.error.code, 'INVALID_MCP_SERVER', 'toggle missing fields code');
assert.equal(byId('toggle-missing-server-name').response.error.message,
  'sessionKey and serverName are required.', 'toggle missing fields message');
assert.equal(byId('toggle-busy').response.error.message, 'Cannot toggle MCP while a turn is active.', 'toggle busy message');
assert.equal(byId('reconnect-busy').response.error.message, 'Cannot reconnect MCP while a turn is active.', 'reconnect busy message');
assert.equal(byId('toggle-missing-session-key').response.error.code, 'INVALID_MCP_SERVER', 'toggle missing key code');
assert.equal(byId('toggle-missing-session-key').response.error.message,
  'sessionKey and serverName are required.', 'toggle missing key message');
assert.equal(byId('toggle-blank-session-key').response.error.code, 'INVALID_MCP_SERVER', 'toggle blank key code');
assert.equal(byId('toggle-blank-session-key').response.error.message,
  'sessionKey and serverName are required.', 'toggle blank key message');
assert.equal(byId('reconnect-missing-session-key').response.error.code, 'INVALID_MCP_SERVER', 'reconnect missing key code');
assert.equal(byId('reconnect-missing-session-key').response.error.message,
  'sessionKey and serverName are required.', 'reconnect missing key message');
assert.deepEqual(byId('toggle-missing-session-key').events, [], 'missing key never closes');
assert.equal(byId('override-auto').response.result.warning,
  'PilotDeck does not expose Claude\'s MCP safety classifier; auto uses conservative ask semantics for this server.',
  'override auto warning');
assert.equal(byId('override-auto').events.length, 2, 'override only closes through the gateway');
assert.equal(byId('override-auto').events[0], 'dispose:v', 'override disposes');
assert.equal(byId('override-auto').events[1], 'evict:v:closed', 'override evicts');
assert.deepEqual(byId('override-auto').post.ask.v, [
  { source: 'session', behavior: 'ask', toolName: 'mcp__alpha__*', force: true },
], 'override auto installs the ask rule');
assert.deepEqual(byId('override-default').response.result, {}, 'override default result');
assert.deepEqual(byId('override-default').post.ask.v, [
  { source: 'session', behavior: 'ask', toolName: 'mcp__alpha__*', force: true },
], 'an explicit default mode still pins the ask rule');
assert.deepEqual(byId('override-null').post.ask.v, [], 'override null clears the rule');
assert.equal(byId('override-invalid-mode').response.error.code, 'INVALID_MCP_PERMISSION_MODE', 'invalid mode code');
assert.equal(byId('override-invalid-mode').response.error.message,
  'Unsupported MCP permission mode: fast', 'invalid mode message');
assert.equal(byId('override-missing-mode').response.error.message,
  'Unsupported MCP permission mode: undefined', 'missing mode message');
assert.equal(byId('override-number-mode').response.error.message,
  'Unsupported MCP permission mode: 42', 'number mode message');
assert.deepEqual(byId('override-invalid-mode').events, [], 'invalid mode never closes');
assert.equal(byId('override-busy-valid').response.error.message,
  'Cannot change MCP permission mode while a turn is active.', 'override busy message');
assert.deepEqual(byId('override-busy-valid').post.ask.vb, [], 'override busy leaves ask empty');
assert.equal(byId('override-busy-invalid-mode').response.error.code, 'INVALID_MCP_PERMISSION_MODE',
  'mode validation precedes the busy gate');
assert.equal(byId('override-missing-session-key').response.error.code, 'INVALID_MCP_SERVER', 'override missing key code');
assert.equal(byId('override-missing-session-key').response.error.message,
  'sessionKey and serverName are required.', 'override missing key message');
assert.equal(byId('override-nonstring-session-key').response.error.message,
  'input.sessionKey?.trim is not a function', 'override non-string key message');
assert.equal(byId('override-close-fault').response.error.message, 'Failed to dispose agent handle.',
  'override close fault message');
assert.deepEqual(byId('override-close-fault').post.ask.f, [
  { source: 'session', behavior: 'ask', toolName: 'mcp__alpha__*', force: true },
], 'override commits before the failing close');
assert.equal(rows.length, 44, 'matrix row count');
for (const row of capabilityCases) {
  assert.equal(row.response.error.code, 'CAPABILITY_UNAVAILABLE', `${row.method} capability code`);
  assert.equal(row.response.error.message, `${row.method} is unavailable.`, `${row.method} capability message`);
}
assert.equal(unknownMethod.ok, false, 'unknown method fails');
assert.equal(unknownMethod.error.code, 'gateway_request_failed', 'unknown method envelope');
assert.equal(unknownMethod.error.message, 'Unknown gateway method mcp_server_pause.', 'unknown method message');

await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({
    sourceCommit, sourceSha256, node: process.version,
    setupKeys: KEYS, cases: rows,
    assemblyCases: { capability: capabilityCases, unknownMethod },
  }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS: ${rows.length} real TS MCP control gateway cases + ${capabilityCases.length + 1} assembly cases`);
