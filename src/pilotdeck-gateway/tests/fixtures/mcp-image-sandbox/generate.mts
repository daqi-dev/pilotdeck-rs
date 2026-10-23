import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.versions.node.split('.')[0], '22', 'generate with Node 22');
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/mcp/runtime/PluginToToolBridge.ts', 'src/context/attachments/AttachmentResolver.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { createMcpToolDefinitionsFromRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const maxImageBytes = Number(readFileSync(join(baseline, sources[1]), 'utf8')
  .match(/const DEFAULT_MAX_IMAGE_BYTES = ([\d_]+);/)![1].replaceAll('_', ''));
assert.equal(maxImageBytes, 5_242_880);
// A real 1x1 PNG; alternative extensions intentionally exercise TS's MIME mapping.
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=', 'base64');
const files = ['pixel.png', 'pixel.JPG', 'pixel.jpeg', 'pixel.gif', 'pixel.webp'].map(path => ({ path, hex: png.toString('hex') }));
const root = mkdtempSync(join(tmpdir(), 'pdrs-mcp-image-sandbox-'));
const cases = [];
try {
  const cwd = join(root, 'cwd'); mkdirSync(cwd);
  for (const file of files) writeFileSync(join(cwd, file.path), Buffer.from(file.hex, 'hex'));
  symlinkSync('pixel.png', join(cwd, 'link.png'));
  let raw;
  const runtime = {
    listAllTools: async () => [{ serverId: 's', toolName: 't', wireName: 'mcp__s__t', description: '', inputSchema: { type: 'object' } }],
    getServerInfo: () => ({ transport: 'stdio', cwd }),
    callTool: async () => ({ content: raw }),
  };
  const [tool] = await createMcpToolDefinitionsFromRuntime(runtime);
  for (const [id, text] of [
    ['normal', 'saved [png](./pixel.png) [jpg](./pixel.JPG) [jpeg](./pixel.jpeg) [gif](./pixel.gif) [webp](./pixel.webp)'],
    ['inside-link', '[local link](./link.png)'],
    ['missing', 'unchanged [missing](./missing.png)'],
  ]) {
    raw = [{ type: 'text', text }];
    const observed = await tool.execute({}, { abortSignal: new AbortController().signal });
    cases.push({ id, raw, observed });
  }
  assert.equal(cases[0].observed.content.length, 6);
  assert.equal(cases[1].observed.content.length, 2);
} finally { rmSync(root, { recursive: true, force: true }); }
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
const output = resolve(process.argv[2]);
assert.notEqual(output, baseline, 'output outside baseline');
assert.ok(!output.startsWith(baseline + '/'), 'never write baseline');
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, maxImageBytes, files, cases }, null, 2) + '\n');
