import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/mcp/runtime/PluginToToolBridge.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { createMcpToolDefinitionsFromRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const root = mkdtempSync(join(tmpdir(), 'pdrs-mcp-boundary-'));
const cases = [];
try {
  const cwd = join(root, 'cwd'); mkdirSync(cwd);
  writeFileSync(join(root, 'outside.png'), 'fixture image');
  symlinkSync('../outside.png', join(cwd, 'link.png'));
  execFileSync('mkfifo', [join(cwd, 'fifo.png')]);
  const writer = join(root, 'writer.cjs');
  writeFileSync(writer, "require('node:fs').writeFileSync(process.argv[2], 'fixture image');");
  let raw;
  const runtime = {
    listAllTools: async () => [{ serverId: 's', toolName: 't', wireName: 'mcp__s__t', description: '', inputSchema: { type: 'object' } }],
    getServerInfo: () => ({ transport: 'stdio', cwd }),
    callTool: async () => ({ content: raw }),
  };
  const [tool] = await createMcpToolDefinitionsFromRuntime(runtime);
  for (const [id, link] of [['parent', '../outside.png'], ['symlink', './link.png'], ['fifo', './fifo.png']]) {
    let child;
    let exited;
    if (id === 'fifo') {
      child = spawn(process.execPath, [writer, join(cwd, 'fifo.png')], { stdio: ['ignore', 'ignore', 'inherit'] });
      exited = new Promise((ok, fail) => { child.on('error', fail); child.on('exit', code => code === 0 ? ok() : fail(new Error(`writer: ${code}`))); });
    }
    raw = [{ type: 'text', text: `[fixture](${link})` }];
    let timerRanBeforeResult = false;
    const timer = setTimeout(() => { timerRanBeforeResult = true; }, 0);
    const observed = await tool.execute({}, { abortSignal: new AbortController().signal });
    clearTimeout(timer);
    cases.push({ id, raw, observed, timerRanBeforeResult });
    if (exited) await exited;
  }
} finally { rmSync(root, { recursive: true, force: true }); }
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
mkdirSync(resolve(process.argv[2]), { recursive: true });
writeFileSync(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
