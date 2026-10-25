// Real TS constructors only. No Rust output is used to manufacture expectations.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const sources = [
  'src/cli/AgentLoopDeploymentProfile.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts',
];
assert.equal(execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), commit);
assert.equal(execFileSync('git', ['-C', baseline, 'status', '--porcelain', '--untracked-files=no'], { encoding: 'utf8' }), '', 'baseline must be clean');
const sourceSha256: Record<string, string> = {};
for (const source of sources) {
  sourceSha256[source] = createHash('sha256').update(await readFile(join(baseline, source))).digest('hex');
}
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
  { name: 'tcp', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000' } },
  { name: 'tcp-timeout', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '25' } },
  { name: 'tcp-invalid-timeout', env: { PILOTDECK_AGENT_LOOP_TRANSPORT: 'tcp', PILOTDECK_AGENT_LOOP_TCP_PORT: '4000', PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS: '0' } },
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

const oracle = { commit, sourceSha256, deployment, lease, connection };
await mkdir(output, { recursive: true });
const flag = 'wx';
await writeFile(join(output, 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`, { flag });
