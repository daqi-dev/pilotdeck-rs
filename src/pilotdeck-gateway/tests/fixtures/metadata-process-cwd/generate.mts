// #879: real entry points, each in a fresh process with a cold cwd cache.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#879 requires Node v22.22.0');
assert.ok(process.env.PDRS_BASELINE, '#879 requires PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const sources = ['src/mcp/config/loadMcpServerConfig.ts', 'src/mcp/runtime/projectMcpSpec.ts',
  'src/mcp/runtime/PluginToToolBridge.ts', 'src/pilot/paths.ts'];
const cases = ['relative', 'empty', 'absolute-base', 'absolute-path']
  .flatMap(kind => ['normal', 'deleted'].map(state => `${kind}-${state}`));
const entries = ['resolve', 'global', 'project', 'pilot-project', 'funasr', 'funasr-home', 'browser', 'bridge', 'load', 'load-project'];

if (process.argv[2] === '--child') {
  const [name, entry] = process.argv.slice(3);
  assert.ok(cases.includes(name) && entries.includes(entry), '#879 unknown probe');
  delete process.env.PILOTDECK_RUNTIME_ROOT;
  const config = await import(pathToFileURL(join(baseline, sources[0])).href);
  const { patchProjectScopedMcpSpec } = await import(pathToFileURL(join(baseline, sources[1])).href);
  const { createMcpToolDefinitionsFromRuntime } = await import(pathToFileURL(join(baseline, sources[2])).href);
  const { getPilotProjectConfigFilePath } = await import(pathToFileURL(join(baseline, sources[3])).href);
  const root = await realpath(await mkdtemp(join(tmpdir(), 'pdrs-metadata-process-')));
  const cwd = join(root, 'cwd');
  const project = join(root, 'project');
  await mkdir(cwd, { recursive: true });
  await mkdir(project, { recursive: true });
  await writeFile(join(project, 'image.png'), 'fixture-image', { flag: 'wx' });
  const kind = name.replace(/-(normal|deleted)$/, '');
  const base = kind === 'absolute-base' ? `${root}/project/../project` : 'unused/../';
  const path = kind === 'absolute-path' ? `${root}/project/../project` : kind === 'empty' ? '' : '../project';
  const input = kind === 'absolute-base' ? base : path;
  const raw = [{ type: 'text', text: '[shot](./image.png)' }];
  const [tool] = await createMcpToolDefinitionsFromRuntime({
    async listAllTools() { return [{ serverId: 'fixture', toolName: 'shot', wireName: 'mcp__fixture__shot', inputSchema: {} }]; },
    getServerInfo() { return { transport: 'stdio', cwd: input }; },
    async callTool() { return { content: raw }; },
  } as any);
  const originalCwd = process.cwd;
  let cwdReads = 0;
  let outcome: unknown;
  process.chdir(cwd);
  if (name.endsWith('-deleted')) await rm(cwd, { recursive: true });
  process.cwd = () => { cwdReads++; return originalCwd(); };
  try {
    if (entry === 'resolve') outcome = { path: resolve(base, path) };
    else if (entry === 'global') outcome = { path: config.getGlobalMcpConfigFilePath(input) };
    else if (entry === 'project') outcome = { path: config.getProjectMcpConfigFilePath(input) };
    else if (entry === 'pilot-project') outcome = { path: getPilotProjectConfigFilePath(input) };
    else if (entry === 'load') outcome = config.loadMcpServerConfig(input, input);
    else if (entry === 'load-project') outcome = config.loadMcpServerConfig(input, project);
    else if (entry === 'bridge') outcome = { content: (await tool.execute({}, {} as any)).content };
    else {
      const patched = patchProjectScopedMcpSpec({ id: entry === 'browser' ? 'browser-use' : 'funasr',
        transport: 'stdio', command: '__PILOTDECK_PROJECT_ROOT__',
        args: ['__PILOTDECK_FUNASR_RUNTIME_ROOT__'] }, entry === 'funasr-home' ? project : input, input);
      // Host-injected executable/install paths are outside this path seam.
      outcome = { cwd: patched.cwd, command: patched.command, args: patched.args };
    }
  } catch (error: any) {
    outcome = { error: { name: error.name, code: error.code, message: error.message, syscall: error.syscall } };
  } finally {
    process.cwd = originalCwd;
    process.chdir(root);
    await rm(root, { recursive: true });
  }
  process.stdout.write(JSON.stringify({ cwdReads, outcome }).split(root).join('<root>') + '\n');
} else {
  assert.ok(process.argv[2], '#879 output directory required');
  const output = resolve(process.argv[2]);
  const sourceCommit = assertBaseline(baseline);
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const rows = cases.map(name => {
    const row: Record<string, unknown> = { name };
    for (const entry of entries) {
      const raw = execFileSync(process.execPath, [
        '--disable-warning=ExperimentalWarning', '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
        fileURLToPath(import.meta.url), '--child', name, entry,
      ], { encoding: 'utf8', timeout: 30_000 });
      row[entry] = JSON.parse(raw);
    }
    process.stdout.write(JSON.stringify(row) + '\n');
    return row;
  });
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({
    node: process.version, sourceCommit, sourceSha256, cases: rows,
  }, null, 2) + '\n', { flag: 'wx' });
}
