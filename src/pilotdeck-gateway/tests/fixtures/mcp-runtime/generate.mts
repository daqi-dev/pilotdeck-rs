// Records the fixed TypeScript baseline's MCP runtime layer:
// src/mcp/runtime/{McpRuntime,PluginToToolBridge,SessionMcpRuntimeRegistry,
// ProjectMcpRuntimeProvider,parsePluginMcpServers,projectMcpSpec}.ts and
// src/tool/builtin/{mcpTool,mcpResources}.ts. Live scenarios drive the real
// McpRuntime against pr10a's scripted stdio server
// (../mcp-client/mock_server.mjs); ownership scenarios inject scripted fake
// runtimes, exactly as the TS options allow.
//
// Run from the repo root with Node 22 and the baseline's installed tsx loader:
//   PDRS_BASELINE=/abs/path/PilotDeck LANG=en_US.UTF-8 \
//     node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/mcp-runtime/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/mcp-runtime
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const MOCK = fileURLToPath(new URL('../mcp-client/mock_server.mjs', import.meta.url));
const ENTRIES = [
  'src/mcp/runtime/McpRuntime.ts',
  'src/mcp/runtime/PluginToToolBridge.ts',
  'src/mcp/runtime/SessionMcpRuntimeRegistry.ts',
  'src/mcp/runtime/ProjectMcpRuntimeProvider.ts',
  'src/tool/builtin/mcpTool.ts',
  'src/tool/builtin/mcpResources.ts',
];

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
const trackedChanges = execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'],
  { env: gitEnv }).toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, ENTRIES);
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const sdkVersion = JSON.parse(readFileSync(
  join(baseline, 'node_modules/@modelcontextprotocol/sdk/package.json'), 'utf8')).version;
assert.equal(sdkVersion, '1.29.0');

// Fixed environment for placeholder expansion.
process.env.PDRS_MCP_SET = 'value';
process.env.PDRS_MCP_NESTED = '${env:PDRS_MCP_SET}';
process.env.HOME = '/pdrs-home';
delete process.env.PDRS_MCP_UNSET;
delete process.env.PILOTDECK_RUNTIME_ROOT;

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { McpRuntime } = await load('src/mcp/runtime/McpRuntime.ts');
const { createMcpToolDefinitionsFromRuntime } = await load('src/mcp/runtime/PluginToToolBridge.ts');
const { SessionMcpRuntimeRegistry } = await load('src/mcp/runtime/SessionMcpRuntimeRegistry.ts');
const { ProjectMcpRuntimeProvider } = await load('src/mcp/runtime/ProjectMcpRuntimeProvider.ts');
const { parsePluginMcpServers } = await load('src/mcp/runtime/parsePluginMcpServers.ts');
const { patchProjectScopedMcpSpec, getPilotDeckInstallCommand } = await load('src/mcp/runtime/projectMcpSpec.ts');
const { createMcpTool } = await load('src/tool/builtin/mcpTool.ts');
const { createListMcpResourcesTool, createReadMcpResourceTool } = await load('src/tool/builtin/mcpResources.ts');

const runRoot = realpathSync(mkdtempSync(join(tmpdir(), 'pdrs-mcp-runtime-')));
function removeOwned(path: string) {
  const rel = relative(runRoot, path);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`remove refused: ${path} is outside the run directory ${runRoot}`);
  }
  rmSync(path, { recursive: true, force: true });
}
const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));

/** Values that differ per machine/run, as `[text, placeholder]` (longest first). */
let scrubs: [string, string][] = [];
function scrub(value: any): any {
  if (typeof value === 'string') {
    let out = value;
    for (const [text, placeholder] of scrubs) out = out.split(text).join(placeholder);
    return out;
  }
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = scrub(value[key]);
    return out;
  }
  return value;
}
function fill(value: any, dir: string): any {
  if (typeof value === 'string') return value.split('<dir>').join(dir).split('<mock>').join(MOCK);
  if (Array.isArray(value)) return value.map((item) => fill(item, dir));
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = fill(value[key], dir);
    return out;
  }
  return value;
}
const json = (value: any) => (value === undefined ? undefined : JSON.parse(JSON.stringify(value)));

function errorValue(error: any): any {
  const out: Record<string, any> = {
    name: error?.name ?? null,
    code: error?.code ?? null,
    message: String(error?.message),
  };
  if (error?.details !== undefined) out.details = json(error.details);
  if (Array.isArray(error?.errors)) out.causes = error.errors.map(errorValue);
  return out;
}

function descriptor(tool: any) {
  return {
    name: tool.name,
    description: tool.description,
    kind: tool.kind,
    inputSchema: tool.inputSchema,
    maxResultBytes: tool.maxResultBytes ?? null,
    aliases: tool.aliases ?? null,
    readOnly: tool.isReadOnly?.({}) ?? null,
    concurrencySafe: tool.isConcurrencySafe?.({}) ?? null,
    destructive: tool.isDestructive?.({}) ?? null,
    openWorld: tool.isOpenWorld?.({}) ?? null,
  };
}

async function execute(tool: any, input: any, abort?: { pre?: boolean; afterMs?: number }) {
  const controller = new AbortController();
  if (abort?.pre) controller.abort();
  if (abort?.afterMs !== undefined) setTimeout(() => controller.abort(), abort.afterMs);
  try {
    const result = await tool.execute(input, { abortSignal: controller.signal });
    return { ok: json(result) };
  } catch (error) {
    return { error: errorValue(error) };
  }
}

// ---------------------------------------------------------------------------
// A. Live runtime scenarios against the scripted stdio server.
// ---------------------------------------------------------------------------
const INIT = [{
  send: {
    jsonrpc: '2.0', id: '$id',
    result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'mock', version: '1' } },
  },
}];
const ok = (result: unknown) => [{ send: { jsonrpc: '2.0', id: '$id', result } }];
const text = (value: string) => ({ content: [{ type: 'text', text: value }] });
const PNG = Buffer.from('89504e470d0a1a0a', 'hex');

type Server = { id: string; proc: Record<string, unknown>[]; spec?: Record<string, unknown> };
type LiveOp = Record<string, any> & { op: string };
type LiveScenario = {
  id: string;
  options?: { connectConcurrency?: number; callTimeoutMs?: number };
  servers: Server[];
  files?: Record<string, string>;
  ops: LiveOp[];
};

const A_TOOLS = {
  tools: [
    { name: 'echo', description: 'echo', inputSchema: { type: 'object', properties: { q: { type: 'string' } } },
      annotations: { readOnlyHint: true, openWorldHint: false } },
    { name: 'rm', inputSchema: { type: 'object' }, annotations: { destructiveHint: true } },
    { name: 'shot', inputSchema: { type: 'object', additionalProperties: false } },
    { name: 'fail', inputSchema: { type: 'object' } },
    { name: 'slow', inputSchema: { type: 'object' } },
    { name: 'crash', inputSchema: { type: 'object' } },
  ],
};
const liveScenarios: LiveScenario[] = [
  {
    id: 'multi-server',
    options: { connectConcurrency: 1 },
    files: { 'a/work/shot.png': PNG.toString('base64'), 'a/work/pic.JPG': PNG.toString('base64') },
    servers: [
      {
        id: 'a',
        spec: { cwd: '<dir>/a/work' },
        proc: [{
          initialize: INIT,
          'tools/list': ok(A_TOOLS),
          'tools/call:echo': ok({ content: [
            { type: 'text', text: 'hi' },
            { type: 'image', data: 'QUJD', mimeType: 'image/png' },
            { type: 'resource', resource: { uri: 'file:///x', text: 't' } },
            { type: 'resource_link', uri: 'u', name: 'n' },
          ] }),
          'tools/call:shot': ok({ content: [
            { type: 'text', text: 'saved [shot](./shot.png) and [missing](./nope.png)' },
            { type: 'text', text: '![p](./pic.JPG) ignored [x](../outside.gif)' },
          ] }),
          'tools/call:fail': ok({ content: [{ type: 'text', text: 'bad' }, { type: 'text', text: 'worse' }], isError: true }),
        }],
      },
      {
        id: 'b',
        proc: [{
          initialize: INIT,
          'tools/list': ok({ tools: [{ name: 'echo', inputSchema: { type: 'object' } }] }),
          'tools/call:echo': ok({ content: [], isError: true }),
        }],
      },
      { id: 'c', spec: { command: '/nonexistent/pdrs-mcp' }, proc: [{}] },
      { id: 'd', proc: [{ initialize: [{ send: { jsonrpc: '2.0', id: '$id', error: { code: -32603, message: 'boom' } } }] }] },
    ],
    ops: [
      { op: 'statuses' },
      { op: 'start' },
      { op: 'statuses' },
      { op: 'serverInfo', id: 'a' },
      { op: 'serverInfo', id: 'c' },
      { op: 'serverInfo', id: 'zz' },
      { op: 'definitions' },
      { op: 'execute', tool: 'mcp__a__echo', input: { q: 'x' } },
      { op: 'execute', tool: 'mcp__a__shot', input: {} },
      { op: 'execute', tool: 'mcp__a__fail', input: {} },
      { op: 'execute', tool: 'mcp__b__echo', input: null },
      { op: 'callTool', server: 'zz', name: 'x', input: {} },
      { op: 'stop' },
      { op: 'statuses' },
    ],
  },
  {
    id: 'parallel-start',
    servers: ['p', 'q', 'r'].map((id) => ({
      id,
      proc: [{ initialize: INIT, 'tools/list': ok({ tools: [{ name: `t-${id}`, inputSchema: { type: 'object' } }] }) }],
    })),
    ops: [{ op: 'start', sorted: true }, { op: 'definitions' }, { op: 'stop' }],
  },
  {
    id: 'timeout-crash',
    options: { connectConcurrency: 1, callTimeoutMs: 300 },
    servers: [{
      id: 'a',
      proc: [
        { initialize: INIT, 'tools/list': ok(A_TOOLS), 'tools/call:crash': [{ exit: 3 }] },
        { initialize: INIT, 'tools/list': ok(A_TOOLS), 'tools/call:slow': [] },
        { initialize: INIT, 'tools/list': ok(A_TOOLS), 'tools/call:echo': ok(text('respawned')) },
      ],
    }],
    ops: [
      { op: 'start' },
      { op: 'definitions' },
      { op: 'execute', tool: 'mcp__a__crash', input: {} },
      { op: 'execute', tool: 'mcp__a__echo', input: {} },
      { op: 'statuses' },
      { op: 'stop' },
      { op: 'start' },
      { op: 'execute', tool: 'mcp__a__slow', input: {} },
      { op: 'statuses' },
      { op: 'execute', tool: 'mcp__a__echo', input: {} },
      { op: 'stop' },
    ],
  },
  {
    id: 'abort',
    options: { connectConcurrency: 1 },
    servers: [{
      id: 'a',
      proc: [
        { initialize: INIT, 'tools/list': ok(A_TOOLS), 'tools/call:slow': [] },
        { initialize: INIT, 'tools/list': ok(A_TOOLS), 'tools/call:echo': ok(text('after abort')) },
      ],
    }],
    ops: [
      { op: 'start' },
      { op: 'definitions' },
      { op: 'execute', tool: 'mcp__a__echo', input: {}, abort: { pre: true } },
      { op: 'execute', tool: 'mcp__a__slow', input: {}, abort: { afterMs: 150 } },
      { op: 'statuses' },
      { op: 'execute', tool: 'mcp__a__echo', input: {} },
      { op: 'stop' },
    ],
  },
  // #361: outputSchema validation through the real client.
  {
    id: 'output-schema',
    options: { connectConcurrency: 1 },
    servers: [{
      id: 'a',
      proc: [{
        initialize: INIT,
        'tools/list': ok({ tools: [{
          name: 'typed',
          inputSchema: { type: 'object' },
          outputSchema: { type: 'object', properties: { a: { type: 'string' }, n: { type: 'integer', minimum: 0 } }, required: ['a'], additionalProperties: false },
        }] }),
        'tools/call:typed': [
          { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[],"structuredContent":{"a":1,"n":-1.5,"x":true}}}' },
        ],
      }],
    }],
    ops: [{ op: 'start' }, { op: 'definitions' }, { op: 'execute', tool: 'mcp__a__typed', input: {} }, { op: 'stop' }],
  },
  {
    id: 'output-schema-ok',
    options: { connectConcurrency: 1 },
    servers: [{
      id: 'a',
      proc: [{
        initialize: INIT,
        'tools/list': ok({ tools: [{
          name: 'typed', inputSchema: { type: 'object' },
          outputSchema: { type: 'object', properties: { a: { type: 'string', format: 'email' } }, required: ['a'] },
        }] }),
        'tools/call:typed': ok({ content: [{ type: 'text', text: 'ok' }], structuredContent: { a: 'x@example.com' } }),
      }],
    }],
    ops: [{ op: 'start' }, { op: 'definitions' }, { op: 'execute', tool: 'mcp__a__typed', input: {} }, { op: 'stop' }],
  },
  // #361: two calls in flight on one connection, answered out of order.
  {
    id: 'concurrent-calls',
    options: { connectConcurrency: 1 },
    servers: [{
      id: 'a',
      proc: [{
        initialize: INIT,
        'tools/list': ok(A_TOOLS),
        // Ids are per connection: initialize 0, tools/list 1, slow 2, echo 3.
        'tools/call:slow': [],
        'tools/call:echo': [
          { line: '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"echo answered first"}]}}' },
          { line: '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"slow answered second"}]}}' },
        ],
      }],
    }],
    ops: [
      { op: 'start' },
      { op: 'definitions' },
      { op: 'parallel', ops: [
        { op: 'execute', tool: 'mcp__a__slow', input: { n: 1 } },
        { op: 'execute', tool: 'mcp__a__echo', input: { n: 2 }, delayMs: 100 },
      ] },
      { op: 'stop' },
    ],
  },
];

function readLogs(dir: string) {
  const logs: any[] = [];
  for (let index = 0; existsSync(join(dir, `log-${index}.jsonl`)); index += 1) {
    const lines = readFileSync(join(dir, `log-${index}.jsonl`), 'utf8').split('\n').filter(Boolean);
    logs.push(lines.map((line) => scrub(JSON.parse(line))));
  }
  return logs;
}
async function settle(dirs: string[]) {
  for (let waited = 0; waited < 10_000; waited += 25) {
    if (dirs.every((dir) => readLogs(dir).every((log) => log.some((entry: any) => entry.event === 'exit')))) return;
    await sleep(25);
  }
  throw new Error('mock processes did not settle');
}

async function runLiveOp(state: { runtime: any; tools: any[]; callTimeoutMs?: number }, op: LiveOp): Promise<any> {
  if (op.delayMs) await sleep(op.delayMs);
  try {
    switch (op.op) {
      case 'start': {
        const statuses = await state.runtime.start();
        return { ok: op.sorted ? [...statuses].sort((x: any, y: any) => (x.serverId < y.serverId ? -1 : 1)) : statuses };
      }
      case 'statuses':
        return { ok: state.runtime.statuses() };
      case 'serverInfo':
        return { ok: state.runtime.getServerInfo(op.id) ?? null };
      case 'definitions':
        state.tools = await createMcpToolDefinitionsFromRuntime(state.runtime,
          state.callTimeoutMs === undefined ? undefined : { callTimeoutMs: state.callTimeoutMs });
        return { ok: state.tools.map(descriptor) };
      case 'execute': {
        const tool = state.tools.find((candidate) => candidate.name === op.tool);
        return await execute(tool, op.input, op.abort);
      }
      case 'callTool':
        return { ok: await state.runtime.callTool(op.server, op.name, op.input) };
      case 'stop':
        await state.runtime.stop();
        return { ok: null };
      case 'parallel':
        return { ok: await Promise.all(op.ops.map((inner: LiveOp) => runLiveOp(state, inner))) };
      default:
        throw new Error(`unknown op ${op.op}`);
    }
  } catch (error) {
    return { error: errorValue(error) };
  }
}

const live: any[] = [];
for (const scenario of liveScenarios) {
  const dir = join(runRoot, 'live', scenario.id);
  mkdirSync(dir, { recursive: true });
  scrubs = [[dir, '<dir>']];
  for (const [path, base64] of Object.entries(scenario.files ?? {})) {
    mkdirSync(join(dir, path, '..'), { recursive: true });
    writeFileSync(join(dir, path), Buffer.from(base64, 'base64'));
  }
  const specs = scenario.servers.map((server) => {
    const serverDir = join(dir, server.id);
    mkdirSync(serverDir, { recursive: true });
    writeFileSync(join(serverDir, 'script.json'), JSON.stringify({ procs: server.proc }));
    return fill({
      id: server.id,
      transport: 'stdio',
      command: 'node',
      args: ['<mock>'],
      env: { MOCK_DIR: `<dir>/${server.id}` },
      ...server.spec,
    }, dir);
  });
  const runtime = new McpRuntime(specs, {
    connectConcurrency: scenario.options?.connectConcurrency,
  });
  const state = { runtime, tools: [] as any[], callTimeoutMs: scenario.options?.callTimeoutMs };
  const steps: any[] = [];
  for (const op of scenario.ops) {
    steps.push({ step: op, ...scrub(json(await runLiveOp(state, op))) });
  }
  const dirs = scenario.servers.map((server) => join(dir, server.id));
  await settle(dirs);
  live.push({
    id: scenario.id,
    options: scenario.options ?? {},
    servers: scenario.servers,
    files: scenario.files ?? {},
    steps,
    processes: Object.fromEntries(scenario.servers.map((server) => [server.id, readLogs(join(dir, server.id))])),
  });
}

// #361 close during a handshake: recorded, then contrasted by the Rust test
// (intentional difference: Rust closes the late connection).
const handshakeClose = await (async () => {
  const dir = join(runRoot, 'live', 'close-during-handshake');
  mkdirSync(dir, { recursive: true });
  scrubs = [[dir, '<dir>']];
  writeFileSync(join(dir, 'script.json'), JSON.stringify({ procs: [{ initialize: [{ sleep: 200 }, ...INIT] }] }));
  const runtime = new McpRuntime([fill({ id: 'a', transport: 'stdio', command: 'node', args: ['<mock>'], env: { MOCK_DIR: '<dir>' } }, dir)]);
  const started = runtime.start();
  await sleep(50);
  await runtime.stop();
  const afterClose = runtime.statuses();
  const statuses = await started;
  await sleep(100);
  const logs = readLogs(dir);
  const exited = logs.every((log: any[]) => log.some((entry) => entry.event === 'exit'));
  await runtime.stop();
  return { afterClose, statuses, after: runtime.statuses(), exitedBeforeSecondStop: exited };
})();

// ---------------------------------------------------------------------------
// B. Bridge projection over a scripted fake runtime port.
// ---------------------------------------------------------------------------
const bridgeDir = join(runRoot, 'bridge');
mkdirSync(join(bridgeDir, 'cwd'), { recursive: true });
writeFileSync(join(bridgeDir, 'cwd', 'a.webp'), PNG);
writeFileSync(join(bridgeDir, 'cwd', 'b.gif'), PNG);
const spec = (serverId: string, toolName: string, extra: Record<string, unknown> = {}) => ({
  serverId, toolName, wireName: `mcp__${serverId}__${toolName}`, description: `d ${toolName}`,
  inputSchema: { type: 'object' }, ...extra,
});
const BRIDGE_SPECS = [
  spec('s', 'plain'),
  spec('s', 'ro', { annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false } }),
  spec('s', 'flags', { annotations: { readOnlyHint: 'yes', destructiveHint: true, openWorldHint: 0 } }),
  spec('s', 'schemaArray', { inputSchema: [1] }),
  spec('s', 'schemaString', { inputSchema: { type: 'string' } }),
  spec('s', 'schemaNull', { inputSchema: null }),
  spec('h', 'remote'),
  spec('gone', 'tool'),
];
type BridgeCase = { tool: string; result?: any; throws?: any };
const bridgeCases: BridgeCase[] = [
  { tool: 'mcp__s__plain', result: { content: 'not an array' } },
  { tool: 'mcp__s__plain', result: { content: [] } },
  { tool: 'mcp__s__plain', result: { content: [null, 7, { type: 3 }, { notype: true }] } },
  { tool: 'mcp__s__plain', result: { content: [{ type: 'text', text: 5 }, { type: 'image', data: 'x' }, { type: 'audio', data: 'a', mimeType: 'm' }] } },
  { tool: 'mcp__s__plain', result: { content: [{ type: 'text', text: 'see [a](./a.webp) [b](./b.gif) [c](./c.png)' }, { type: 'text', text: '[up](./../cwd/b.gif)' }] } },
  { tool: 'mcp__s__plain', result: { content: [{ type: 'image', data: 'QQ==', mimeType: 'image/x' }, { type: 'text', text: '[a](./a.webp)' }] } },
  { tool: 'mcp__h__remote', result: { content: [{ type: 'text', text: '[a](./a.webp)' }] } },
  { tool: 'mcp__s__plain', result: { content: [{ type: 'image', data: 'QQ==', mimeType: 'image/x' }], isError: true } },
  { tool: 'mcp__s__plain', result: { content: 'x', isError: true } },
  { tool: 'mcp__s__plain', result: { content: [{ type: 'text', text: 'a' }, { type: 'image' }, { type: 'text', text: 'b' }], isError: true } },
  { tool: 'mcp__s__plain', result: { content: [], isError: false } },
  { tool: 'mcp__s__plain', throws: { code: 'mcp_call_timeout', message: 'MCP call timed out (server=s): late' } },
  { tool: 'mcp__s__plain', throws: { code: 'mcp_session_expired', message: 'expired' } },
  { tool: 'mcp__s__plain', throws: { code: 'mcp_server_not_registered', message: 'MCP server s is not registered' } },
  { tool: 'mcp__s__plain', throws: { code: -32000, message: 'MCP error -32000: Session expired again' } },
  { tool: 'mcp__s__plain', throws: { message: 'no code' } },
  { tool: 'mcp__gone__tool', result: { content: [] } },
];
const bridgeCalls: any[] = [];
let nextBridge: BridgeCase | undefined;
const fakePort = {
  start: async () => [],
  stop: async () => {},
  listAllTools: async () => json(BRIDGE_SPECS),
  getServerInfo: (serverId: string) => (serverId === 's'
    ? { transport: 'stdio', cwd: join(bridgeDir, 'cwd') }
    : serverId === 'h' ? { transport: 'streamable_http' } : undefined),
  callTool: async (serverId: string, toolName: string, input: unknown, options: any) => {
    bridgeCalls.push({ serverId, toolName, input, timeoutMs: options?.timeoutMs ?? null, signal: options?.signal !== undefined });
    const current = nextBridge!;
    if (current.throws) {
      const error = new Error(current.throws.message) as any;
      if (current.throws.code !== undefined) error.code = current.throws.code;
      throw error;
    }
    return current.result;
  },
};
scrubs = [[bridgeDir, '<dir>']];
const bridgeTools = await createMcpToolDefinitionsFromRuntime(fakePort, { callTimeoutMs: 1234 });
const bridgeResults: any[] = [];
for (const item of bridgeCases) {
  nextBridge = item;
  const tool = bridgeTools.find((candidate: any) => candidate.name === item.tool);
  const before = bridgeCalls.length;
  const outcome = await execute(tool, { k: 1 });
  bridgeResults.push({ case: item, ...scrub(outcome), calls: scrub(json(bridgeCalls.slice(before))) });
}
const bridge = {
  specs: BRIDGE_SPECS,
  descriptors: bridgeTools.map(descriptor),
  cases: bridgeResults,
};

// ---------------------------------------------------------------------------
// C. parsePluginMcpServers.
// ---------------------------------------------------------------------------
const PARSE_INPUTS: any[] = [
  null,
  'not an object',
  {
    '': { command: 'x' },
    nullish: null,
    str: 'cmd',
    arr: ['node'],
    stdio: { command: '${env:PDRS_MCP_SET}', args: ['~/a', 3, '${env:PDRS_MCP_UNSET}'], env: { A: '~', B: '${env:PDRS_MCP_SET}' }, cwd: '~/w', callTimeoutMs: 5, perSession: true },
    stdioLoose: { command: 'node', args: 'not array', env: { A: 1 }, cwd: 7, callTimeoutMs: -1, perSession: 'yes' },
    stdioArrayEnv: { command: 'node', env: ['x', 'y'], callTimeoutMs: Infinity },
    emptyCommand: { command: '${env:PDRS_MCP_UNSET}', url: 'http://x' },
    http: { url: 'http://h/${env:PDRS_MCP_NESTED}', headers: { Auth: '${env:PDRS_MCP_SET}' } },
    httpUrl: { httpUrl: 'http://alt', transport: 'sse' },
    emptyUrl: { url: '', httpUrl: 'http://ignored' },
    numberUrl: { url: 5, httpUrl: 'http://fallback', headers: { n: 1 } },
    sse: { url: 'http://s', transport: 'sse', headers: [] },
    none: { args: [] },
    2: { command: 'two' },
    1: { url: 'http://one' },
  },
];
const parse = PARSE_INPUTS.map((input) => ({ input, output: json(parsePluginMcpServers(input)) }));

// ---------------------------------------------------------------------------
// D. patchProjectScopedMcpSpec (per-host paths scrubbed to placeholders).
// ---------------------------------------------------------------------------
const installCommand = getPilotDeckInstallCommand();
const playwrightProbe = patchProjectScopedMcpSpec(
  { id: 'browser-use', transport: 'stdio', command: '__PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT__' }, '/p', '/h').command;
const funasrProbe = patchProjectScopedMcpSpec(
  { id: 'funasr', transport: 'stdio', command: '__PILOTDECK_FUNASR_MCP_ENTRYPOINT__' }, '/p', '/h').command;
const pathScrubs: [string, string][] = [
  [installCommand, '<install>'],
  [playwrightProbe, '<playwright>'],
  [funasrProbe, '<funasr>'],
  [process.execPath, '<node>'],
];
const PATCH_INPUTS: any[] = [
  [{ id: 'other', transport: 'stdio', command: '__PILOTDECK_NODE_EXECUTABLE__' }, '/p', '/h'],
  [{ id: 'funasr', transport: 'streamable_http', url: '__PILOTDECK_PROJECT_ROOT__' }, '/p', '/h'],
  [{ id: 'browser-use', transport: 'stdio', command: '__PILOTDECK_NODE_EXECUTABLE__', args: ['__PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT__', '__PILOTDECK_PROJECT_ROOT__'], env: { E: 'v' }, cwd: '/old', perSession: true }, '/proj/./x/..', '/h'],
  [{ id: 'browser-use', transport: 'stdio', command: 'node' }, '/p', '/h'],
  [{ id: 'funasr', transport: 'stdio', command: '__PILOTDECK_NODE_EXECUTABLE__', args: ['__PILOTDECK_FUNASR_MCP_ENTRYPOINT__', '--root=__PILOTDECK_PROJECT_ROOT__', '__PILOTDECK_FUNASR_RUNTIME_ROOT__', '__PILOTDECK_FUNASR_INSTALL_COMMAND__', '__PILOTDECK_PROJECT_ROOT____PILOTDECK_PROJECT_ROOT__'], callTimeoutMs: 9 }, '/proj', '/home/u/'],
  // `replaceAll` with a string replacement applies GetSubstitution: `$&`
  // in the project root expands to the matched marker.
  [{ id: 'funasr', transport: 'stdio', command: 'x', args: ['[__PILOTDECK_PROJECT_ROOT__]'] }, '/p/$&/$$', '/'],
];
const patch = PATCH_INPUTS.map(([specIn, projectRoot, pilotHome]) => {
  scrubs = pathScrubs;
  return { spec: specIn, projectRoot, pilotHome, output: scrub(json(patchProjectScopedMcpSpec(specIn, projectRoot, pilotHome))) };
});

// ---------------------------------------------------------------------------
// E. SessionMcpRuntimeRegistry with scripted runtimes.
// ---------------------------------------------------------------------------
type RegistryOp = Record<string, any> & { op: string };
const registryScenarios: { id: string; runtimes: Record<string, { fail?: string; delayMs?: number }>; ops: RegistryOp[] }[] = [
  {
    id: 'basic',
    runtimes: { r1: {}, r2: {}, r3: {} },
    ops: [
      { op: 'register', key: 's', runtime: 'r1', as: 'g1' },
      { op: 'register', key: 's', runtime: 'r2', as: 'g2' },
      { op: 'register', key: 't', runtime: 'r3', as: 'g3' },
      { op: 'size' },
      { op: 'release', reg: 'g1' },
      { op: 'release', reg: 'g1' },
      { op: 'size' },
      { op: 'dispose' },
      { op: 'state' },
      { op: 'size' },
      { op: 'release', reg: 'g2' },
      { op: 'register', key: 's', runtime: 'r1', as: 'g4' },
      { op: 'dispose' },
    ],
  },
  {
    id: 'failures',
    runtimes: { a: { fail: 'stop a failed' }, b: { fail: 'stop b failed' }, c: { delayMs: 30 } },
    ops: [
      { op: 'register', key: 's', runtime: 'a', as: 'ga' },
      { op: 'register', key: 's', runtime: 'b', as: 'gb' },
      { op: 'register', key: 'u', runtime: 'c', as: 'gc' },
      { op: 'parallel', ops: [{ op: 'dispose' }, { op: 'state' }, { op: 'release', reg: 'gc' }] },
      { op: 'state' },
      { op: 'dispose' },
    ],
  },
  // Same key, released by identity: the later registration first.
  {
    id: 'identity',
    runtimes: { r1: {}, r2: {} },
    ops: [
      { op: 'register', key: 's', runtime: 'r1', as: 'g1' },
      { op: 'register', key: 's', runtime: 'r2', as: 'g2' },
      { op: 'release', reg: 'g2' },
      { op: 'size' },
      { op: 'release', reg: 'g1' },
    ],
  },
  {
    id: 'single-failure',
    runtimes: { a: { fail: 'only one' }, b: {} },
    ops: [
      { op: 'register', key: 's', runtime: 'a', as: 'ga' },
      { op: 'register', key: 's', runtime: 'b', as: 'gb' },
      { op: 'release', reg: 'ga' },
      { op: 'release', reg: 'ga' },
      { op: 'dispose' },
      { op: 'state' },
    ],
  },
];
const registry: any[] = [];
for (const scenario of registryScenarios) {
  const stops: Record<string, number> = {};
  const runtimes: Record<string, any> = {};
  for (const [label, behaviour] of Object.entries(scenario.runtimes)) {
    stops[label] = 0;
    runtimes[label] = {
      stop: async () => {
        stops[label] += 1;
        if (behaviour.delayMs) await sleep(behaviour.delayMs);
        if (behaviour.fail) throw new Error(behaviour.fail);
      },
    };
  }
  const subject = new SessionMcpRuntimeRegistry();
  const registrations: Record<string, any> = {};
  const run = async (op: RegistryOp): Promise<any> => {
    try {
      switch (op.op) {
        case 'register':
          registrations[op.as] = subject.register(op.key, runtimes[op.runtime]);
          return { ok: null };
        case 'release':
          await registrations[op.reg].dispose();
          return { ok: null };
        case 'size':
          return { ok: subject.size };
        case 'state':
          return { ok: subject.state };
        case 'dispose':
          await subject.dispose();
          return { ok: null };
        case 'parallel':
          return { ok: await Promise.all(op.ops.map(run)) };
        default:
          throw new Error(op.op);
      }
    } catch (error) {
      return { error: errorValue(error) };
    }
  };
  const steps: any[] = [];
  for (const op of scenario.ops) {
    const result = await run(op);
    steps.push({ step: op, ...json(result), stops: { ...stops } });
  }
  registry.push({ id: scenario.id, runtimes: scenario.runtimes, steps });
}

// ---------------------------------------------------------------------------
// F. ProjectMcpRuntimeProvider with a scripted runtime factory.
// ---------------------------------------------------------------------------
type ProviderOp = Record<string, any> & { op: string };
type ProviderScenario = {
  id: string;
  config?: { global?: unknown; project?: unknown };
  /** Per created runtime (in creation order): start failures / status errors. */
  runtimes?: { startError?: string; statuses?: Record<string, string>; stopError?: string; startDelayMs?: number }[];
  ops: ProviderOp[];
};
const PLUGINS_A = { a: { command: 'node', args: ['a'] }, b: { url: 'http://b' } };
const PLUGINS_A_REORDERED = { b: { url: 'http://b' }, a: { args: ['a'], command: 'node' } };
const PLUGINS_B = { a: { command: 'node', args: ['a2'] } };
const PLUGINS_MIXED = {
  shared: { command: 'node' },
  per: { command: 'node', perSession: true },
  remote: { url: 'http://r' },
  bad: { foo: 1 },
};
const providerScenarios: ProviderScenario[] = [
  {
    id: 'leases',
    ops: [
      { op: 'acquire', plugins: {}, as: 'empty' },
      { op: 'acquire', plugins: PLUGINS_A, as: 'l1' },
      { op: 'acquire', plugins: PLUGINS_A_REORDERED, as: 'l2' },
      { op: 'generations' },
      { op: 'acquire', plugins: PLUGINS_B, as: 'l3' },
      { op: 'generations' },
      { op: 'release', lease: 'l1' },
      { op: 'release', lease: 'l1' },
      { op: 'generations' },
      { op: 'release', lease: 'l2' },
      { op: 'generations' },
      { op: 'release', lease: 'empty' },
      { op: 'acquire', plugins: PLUGINS_A, as: 'l4' },
      { op: 'dispose' },
      { op: 'state' },
      { op: 'generations' },
      { op: 'release', lease: 'l3' },
      { op: 'release', lease: 'l4' },
      { op: 'acquire', plugins: PLUGINS_A, as: 'l5' },
      { op: 'perSession', plugins: PLUGINS_A, force: false },
      { op: 'dispose' },
    ],
  },
  {
    id: 'per-session',
    ops: [
      { op: 'perSession', plugins: PLUGINS_MIXED, force: false },
      { op: 'perSession', plugins: PLUGINS_MIXED, force: true },
      { op: 'perSession', plugins: {}, force: true },
      { op: 'acquire', plugins: PLUGINS_MIXED, as: 'l' },
      { op: 'acquire', plugins: { per: { command: 'node', perSession: true } }, as: 'onlyPer' },
      { op: 'dispose' },
    ],
  },
  {
    id: 'config',
    config: {
      global: { mcpServers: { g: { command: 'global' }, a: { command: 'global-a' } } },
      project: { mcpServers: { p: { url: 'http://p' }, g: { command: 'project-g' }, browser: 1 } },
    },
    ops: [
      { op: 'acquire', plugins: { a: { command: 'plugin-a' }, z: { command: 'plugin-z' } }, as: 'l' },
      { op: 'perSession', plugins: {}, force: true },
      { op: 'dispose' },
    ],
  },
  {
    id: 'config-invalid',
    config: { global: '{', project: [] },
    ops: [{ op: 'acquire', plugins: { a: { command: 'x' } }, as: 'l' }, { op: 'dispose' }],
  },
  {
    id: 'startup-failures',
    runtimes: [
      { statuses: { a: 'spawn failed', funasr: 'model missing' } },
      { startError: 'runtime start rejected' },
      { stopError: 'stop rejected' },
    ],
    ops: [
      { op: 'acquire', plugins: { a: { command: 'x' }, funasr: { command: 'y' }, ok: { command: 'z' } }, as: 'l1' },
      { op: 'acquire', plugins: { only: { command: 'x' } }, as: 'l2' },
      { op: 'generations' },
      { op: 'acquire', plugins: { s: { command: 'x' } }, as: 'l3' },
      { op: 'release', lease: 'l3' },
      { op: 'release', lease: 'l1' },
      { op: 'dispose' },
    ],
  },
  {
    id: 'concurrent-acquire',
    runtimes: [{ startDelayMs: 40 }, { stopError: 'first' }, { stopError: 'second' }],
    ops: [
      { op: 'parallel', ops: [
        { op: 'acquire', plugins: PLUGINS_A, as: 'l1' },
        { op: 'acquire', plugins: PLUGINS_A_REORDERED, as: 'l2' },
      ] },
      { op: 'generations' },
      { op: 'acquire', plugins: { x: { command: 'x' } }, as: 'l3' },
      { op: 'acquire', plugins: { y: { command: 'y' } }, as: 'l4' },
      { op: 'dispose' },
      { op: 'dispose' },
    ],
  },
];
const provider: any[] = [];
for (const scenario of providerScenarios) {
  const dir = join(runRoot, 'provider', scenario.id);
  const projectRoot = join(dir, 'project');
  const pilotHome = join(dir, 'home');
  mkdirSync(join(projectRoot, '.pilotdeck'), { recursive: true });
  mkdirSync(pilotHome, { recursive: true });
  const write = (path: string, value: unknown) => {
    if (value === undefined) return;
    writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value));
  };
  write(join(pilotHome, 'mcp.json'), scenario.config?.global);
  write(join(projectRoot, '.pilotdeck', 'mcp.json'), scenario.config?.project);
  scrubs = [[dir, '<dir>'], ...pathScrubs];
  let events: any[] = [];
  let created = 0;
  const createRuntime = (servers: any[]) => {
    const label = `R${created}`;
    const behaviour = scenario.runtimes?.[created] ?? {};
    created += 1;
    events.push({ event: 'create', runtime: label, servers: json(servers) });
    return {
      start: async () => {
        events.push({ event: 'start', runtime: label });
        if (behaviour.startDelayMs) await sleep(behaviour.startDelayMs);
        if (behaviour.startError) throw new Error(behaviour.startError);
        return servers.map((server) => behaviour.statuses?.[server.id]
          ? { serverId: server.id, status: 'error', error: behaviour.statuses[server.id] }
          : { serverId: server.id, status: 'ready' });
      },
      stop: async () => {
        events.push({ event: 'stop', runtime: label });
        if (behaviour.stopError) throw new Error(behaviour.stopError);
      },
      listAllTools: async () => servers.map((server) => ({
        serverId: server.id, toolName: 't', wireName: `mcp__${server.id}__t`, description: '', inputSchema: { type: 'object' },
      })),
      getServerInfo: () => undefined,
      callTool: async () => ({ content: [] }),
    };
  };
  let diagnostics: any[] = [];
  const subject = new ProjectMcpRuntimeProvider({
    projectRoot,
    pilotHome,
    createRuntime,
    onDiagnostic: (message: string, error?: unknown) => diagnostics.push(error === undefined ? { message } : { message, error: errorValue(error) }),
  });
  const leases: Record<string, any> = {};
  const run = async (op: ProviderOp): Promise<any> => {
    try {
      switch (op.op) {
        case 'acquire': {
          const lease = await subject.acquireSharedRuntime(op.plugins);
          leases[op.as] = lease;
          return { ok: lease.tools.map((tool: any) => tool.name) };
        }
        case 'release':
          await leases[op.lease].release();
          return { ok: null };
        case 'perSession':
          return { ok: json(subject.getPerSessionServerSpecs(op.plugins, op.force)) ?? null };
        case 'generations':
          return { ok: (subject as any).sharedEntries.size };
        case 'state':
          return { ok: subject.state };
        case 'dispose':
          await subject.dispose();
          return { ok: null };
        case 'parallel':
          return { ok: await Promise.all(op.ops.map(run)) };
        default:
          throw new Error(op.op);
      }
    } catch (error) {
      return { error: errorValue(error) };
    }
  };
  const steps: any[] = [];
  for (const op of scenario.ops) {
    events = [];
    diagnostics = [];
    const result = await run(op);
    // Concurrent stops have no observable order; compare them as a set.
    const ordered = [...events.filter((e) => e.event !== 'stop'), ...events.filter((e) => e.event === 'stop').sort((x, y) => (x.runtime < y.runtime ? -1 : 1))];
    steps.push({ step: op, ...scrub(json(result)), events: scrub(json(ordered)), diagnostics: scrub(json(diagnostics)) });
  }
  provider.push({ id: scenario.id, config: scenario.config ?? {}, runtimes: scenario.runtimes ?? [], steps });
}

// ---------------------------------------------------------------------------
// G. tool/builtin/{mcpTool,mcpResources}.ts.
// ---------------------------------------------------------------------------
const builtinCalls: any[] = [];
const toolAdapter = {
  callTool: async (serverId: string, toolName: string, input: unknown) => {
    builtinCalls.push({ op: 'callTool', serverId, toolName, input });
    if ((input as any)?.fail) throw new Error('adapter failed');
    return { echoed: input };
  },
};
const resourceAdapter = {
  listResources: async (serverId?: unknown) => {
    builtinCalls.push({ op: 'listResources', serverId: serverId ?? null });
    return [{ uri: 'file:///a', serverId: serverId ?? null }];
  },
  readResource: async (serverId: unknown, uri: unknown) => {
    builtinCalls.push({ op: 'readResource', serverId, uri });
    if (uri === 'missing') throw new Error('no such resource');
    return { contents: [{ uri, text: 'body' }] };
  },
};
const NAME_CASES = [['my server', 'a..b'], ['_x_', '__y__'], ['a-b', '工具t'], ['!!', 't'], ['s', '']];
const names = NAME_CASES.map(([serverId, toolName]) => {
  try {
    return { serverId, toolName, ok: descriptor(createMcpTool({ serverId, toolName })) };
  } catch (error) {
    return { serverId, toolName, error: errorValue(error) };
  }
});
const custom = descriptor(createMcpTool({
  serverId: 's', toolName: 't', description: 'custom', inputSchema: { type: 'object', required: ['x'] }, adapter: toolAdapter,
}));
const builtinRuns: any[] = [];
const runBuiltin = async (label: string, tool: any, input: any) => {
  const before = builtinCalls.length;
  builtinRuns.push({ label, input, ...(await execute(tool, input)), calls: json(builtinCalls.slice(before)) });
};
await runBuiltin('mcp-tool', createMcpTool({ serverId: 's', toolName: 't', adapter: toolAdapter }), { q: 1 });
await runBuiltin('mcp-tool-fail', createMcpTool({ serverId: 's', toolName: 't', adapter: toolAdapter }), { fail: true });
await runBuiltin('mcp-tool-no-adapter', createMcpTool({ serverId: 's', toolName: 't' }), {});
await runBuiltin('list', createListMcpResourcesTool(resourceAdapter), {});
await runBuiltin('list-server', createListMcpResourcesTool(resourceAdapter), { serverId: 'srv' });
await runBuiltin('list-number', createListMcpResourcesTool(resourceAdapter), { serverId: 3 });
await runBuiltin('list-no-adapter', createListMcpResourcesTool(), {});
await runBuiltin('read', createReadMcpResourceTool(resourceAdapter), { serverId: 'srv', uri: 'file:///a' });
await runBuiltin('read-missing', createReadMcpResourceTool(resourceAdapter), { serverId: 'srv', uri: 'missing' });
await runBuiltin('read-no-adapter', createReadMcpResourceTool(), { serverId: 'srv', uri: 'x' });
const builtins = {
  names,
  custom,
  list: descriptor(createListMcpResourcesTool()),
  read: descriptor(createReadMcpResourceTool()),
  runs: builtinRuns,
};

removeOwned(runRoot);

// Spot checks.
const liveScenario = (id: string) => live.find((item) => item.id === id)!;
assert.equal(liveScenario('multi-server').steps[1].ok.length, 4);
assert.equal(liveScenario('output-schema').steps[2].error.details.errorCode, 'mcp_call_failed');
assert.match(liveScenario('output-schema').steps[2].error.message, /does not match the tool's output schema/);
assert.equal(handshakeClose.statuses[0].status, 'ready');
const stepCount = [...live, ...registry, ...provider].reduce((sum, item) => sum + item.steps.length, 0);
assert.equal(live.length, 7);
assert.equal(registry.length, 4);
assert.equal(provider.length, 6);
assert.equal(stepCount, 122);
assert.equal(bridge.cases.length, 17);
assert.equal(parse.length, 3);
assert.equal(patch.length, 6);
assert.equal(builtins.runs.length, 10);

const oracle = { sourceCommit, sourceSha256, sdkVersion, live, handshakeClose, bridge, parse, patch, registry, provider, builtins };
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(`PASS: ${live.length + registry.length + provider.length} scenarios, ${stepCount} steps, ${Object.keys(sourceSha256).length} source files`);
process.exit(0);
