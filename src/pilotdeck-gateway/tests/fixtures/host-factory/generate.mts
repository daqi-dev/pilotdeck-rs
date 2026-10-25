// Real TS constructors only. No Rust output is used to manufacture expectations.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sources = [
  'src/cli/AgentLoopDeploymentProfile.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts',
  'src/router/stats/TokenStatsCollector.ts',
  'src/router/utils/modelPricing.ts',
];
const commit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const load = (source: string) => import(pathToFileURL(join(baseline, source)).href);

// -- Deployment profile selection -------------------------------------------------
const { resolveAgentLoopDeploymentProfile } = await load(sources[0]);
const deploymentSpecs = [
  { name: 'default', env: {} },
  { name: 'native', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'native' } },
  { name: 'empty', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: '  ' } },
  { name: 'stdio', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'stdio', PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND: 'node', PILOTDECK_AGENT_LOOP_SIDECAR_PATH: 'side.js', PILOTDECK_AGENT_LOOP_TCP_HOST: '1.2.3.4', PILOTDECK_AGENT_LOOP_TCP_PORT: '7000' } },
  { name: 'stdio-empty-command', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'stdio', PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND: '   ' } },
  { name: 'tcp-missing-port', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp' } },
  { name: 'tcp-invalid-port', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '0' } },
  { name: 'tcp-port-plus', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '+80' } },
  { name: 'tcp-port-float', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '80.0' } },
  { name: 'tcp-port-exponent', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '1e3' } },
  { name: 'tcp-port-hex', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '0x50' } },
  { name: 'tcp-port-padded', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: ' 80 ' } },
  { name: 'tcp-port-empty', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '' } },
  { name: 'tcp-port-blank', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '   ' } },
  { name: 'tcp-port-fraction', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '80.5' } },
  { name: 'tcp-port-max', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '65535' } },
  { name: 'tcp-port-overflow', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '65536' } },
  { name: 'tcp', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000' } },
  { name: 'tcp-timeout', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '25' } },
  { name: 'tcp-invalid-timeout', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '0' } },
  { name: 'tcp-timeout-plus', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '+80' } },
  { name: 'tcp-timeout-float', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '80.0' } },
  { name: 'tcp-timeout-exponent', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '1e3' } },
  { name: 'tcp-timeout-hex', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '0x50' } },
  { name: 'tcp-timeout-padded', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: ' 80 ' } },
  { name: 'tcp-timeout-empty', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '' } },
  { name: 'tcp-timeout-fraction', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '80.5' } },
  { name: 'invalid', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'bogus' } },
];
const deployment = deploymentSpecs.map((spec) => {
  const cwd = '/tmp/pdrs-cwd';
  try {
    return { ...spec, cwd, ok: true, value: resolveAgentLoopDeploymentProfile({ env: spec.env, cwd }) };
  } catch (error) {
    return { ...spec, cwd, ok: false, error: (error as Error).message };
  }
});

// -- Registry lease semantics -----------------------------------------------------
// The real class methods run against a minimal deterministic runtime stub: no
// clock, no I/O. This measures the exact acquire/release/retire/dispose order.
const { ProjectRuntimeRegistry } = await load(sources[1]);
type StubRuntime = {
  runtimeState: string;
  sessionLeases: number;
  disposals: number;
  backgroundSubagents: { shutdown(): void };
  resourcesBundle: { dispose(): Promise<void> };
  resolveSessionDrain: undefined;
};
function registry(): any {
  const value: any = Object.create(ProjectRuntimeRegistry.prototype);
  value.runtimes = new Map();
  value.runtimeDisposals = new Set();
  value.reloadTail = Promise.resolve();
  value.disposed = false;
  return value;
}
function runtime(): StubRuntime {
  const value: StubRuntime = {
    runtimeState: 'active',
    sessionLeases: 0,
    disposals: 0,
    backgroundSubagents: { shutdown() {} },
    resourcesBundle: { async dispose() { value.disposals += 1; } },
    resolveSessionDrain: undefined,
  };
  return value;
}
const tick = () => new Promise(setImmediate);
const lease: any[] = [];
{
  const owner = registry();
  const generation = runtime();
  const first = owner.acquireRuntimeLease(generation);
  const second = owner.acquireRuntimeLease(generation);
  lease.push({ name: 'two-leases', leases: generation.sessionLeases, disposals: generation.disposals, state: generation.runtimeState });
  await first();
  lease.push({ name: 'after-first-release', leases: generation.sessionLeases, disposals: generation.disposals, state: generation.runtimeState });
  owner.retireRuntime(generation);
  lease.push({ name: 'retire-with-second-lease', leases: generation.sessionLeases, disposals: generation.disposals, state: generation.runtimeState });
  await second();
  await tick();
  lease.push({ name: 'after-last-release', leases: generation.sessionLeases, disposals: generation.disposals, state: generation.runtimeState });
  await second();
  await tick();
  lease.push({ name: 'after-double-release', leases: generation.sessionLeases, disposals: generation.disposals, state: generation.runtimeState });
}
{
  const owner = registry();
  const generation = runtime();
  const held = owner.acquireRuntimeLease(generation);
  generation.runtimeState = 'retired';
  let error = null;
  try { owner.acquireRuntimeLease(generation); } catch (caught) { error = (caught as Error).message; }
  lease.push({ name: 'acquire-retired', error, leases: generation.sessionLeases });
  await held();
  await tick();
}

// -- Configured router pricing through the turn budget ---------------------------
// `AgentTurnCapabilities.model.budget.estimateUsageCost` forwards to
// `RouterRuntime.estimateUsageCost`, whose `TokenStatsCollector` holds the
// parsed `router.stats.modelPricing` table (`RouterRuntime.ts:1065-1067`).
const { TokenStatsCollector } = await load('src/router/stats/TokenStatsCollector.ts');
const pricing = (() => {
  const modelPricing = { 'test/mock': { input: 7, output: 11, cacheRead: 3 } };
  const usage = {
    inputTokens: 1_000_000,
    outputTokens: 2_000_000,
    cacheReadTokens: 3_000_000,
    cacheWriteTokens: 4_000_000,
  };
  const collector = new TokenStatsCollector({ enabled: false, modelPricing });
  return {
    modelPricing,
    usage,
    provider: 'test',
    model: 'mock',
    costUsd: collector.estimateCost(usage, 'test', 'mock'),
  };
})();

// -- stdio connection errors ------------------------------------------------------
const { createStdioAgentLoopSidecarConnectionFactory } = await load(sources[2]);
const connection: any[] = [];
{
  const connect = createStdioAgentLoopSidecarConnectionFactory({ command: '/nonexistent-pdrs-sidecar', args: [] });
  const open = connect();
  try { for await (const _ of open.receive()) {} connection.push({ name: 'spawn-failure', ok: true }); }
  catch (error) { connection.push({ name: 'spawn-failure', ok: false, error: (error as Error).message }); }
  await open.close();
}
{
  const connect = createStdioAgentLoopSidecarConnectionFactory({
    command: 'python3',
    args: ['-c', 'import sys; sys.exit(7)'],
  });
  const open = connect();
  try { for await (const _ of open.receive()) {} connection.push({ name: 'exit-before-terminal', ok: true }); }
  catch (error) { connection.push({ name: 'exit-before-terminal', ok: false, error: (error as Error).message }); }
  await open.close();
}

const oracle = { commit, sourceSha256, deployment, lease, pricing, connection };
await mkdir(output, { recursive: true });
const flag = 'wx';
await writeFile(join(output, 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`, { flag });
