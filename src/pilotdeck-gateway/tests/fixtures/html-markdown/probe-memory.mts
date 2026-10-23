// #454: real production fetch -> domino -> Turndown, without network I/O.
// Run each shape in a fresh Node 22 process under time -l and timeout 900s.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.match(process.version, /^v22\./);
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
const base = resolve(process.env.PDRS_BASELINE);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(base, ref, git);
const sourceSha256 = sourceClosureSha256(base, ref, git, ['src/tool/builtin/web/urlFetcher.ts']);
const original = JSON.parse(readFileSync(new URL('./oracle.json', import.meta.url), 'utf8'));
const require = createRequire(realpathSync(join(base, 'node_modules/turndown/lib/turndown.cjs.js')));
for (const [name, spec] of Object.entries(original.dependencies) as [string, any][]) {
  const directory = dirname(require.resolve(`${name}/package.json`));
  assert.equal(JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')).version, spec.version);
  for (const [file, hash] of Object.entries(spec.sha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(directory, file))).digest('hex'), hash, `${name}/${file}`);
  }
}
const shape = process.argv[2];
assert.ok(shape === 'br' || shape === 'a-br');
const unit = shape === 'br' ? '<br>' : 'a<br>';
const html = unit.repeat(10 * 1024 * 1024 / unit.length);
assert.equal(Buffer.byteLength(html), 10 * 1024 * 1024);
const sha256 = (value: string) => createHash('sha256').update(value).digest('hex');
const source = (file: string) => pathToFileURL(join(base, 'src', file)).href;
const { getURLMarkdownContent } = await import(source('tool/builtin/web/urlFetcher.ts'));
const { WebFetchUrlCache } = await import(source('tool/builtin/web/urlContentCache.ts'));
console.error(JSON.stringify({ stage: 'start', shape, node: process.version, ref, sourceSha256,
  htmlBytes: Buffer.byteLength(html), htmlSha256: sha256(html) }));
const start = performance.now();
try {
  const result = await getURLMarkdownContent('https://example.com/memory-probe', new AbortController().signal, {
    cache: new WebFetchUrlCache(),
    fetchHook: async () => ({ status: 200, statusText: 'OK', headers: { 'content-type': 'text/html' },
      arrayBuffer: async () => Uint8Array.from(Buffer.from(html)).buffer }),
  });
  assert.ok('content' in result);
  console.log(JSON.stringify({ shape, status: 'success', seconds: (performance.now() - start) / 1000,
    markdownBytes: Buffer.byteLength(result.content), markdownUtf16Length: result.content.length,
    markdownSha256: sha256(result.content) }));
} catch (error) {
  console.log(JSON.stringify({ shape, status: 'error', seconds: (performance.now() - start) / 1000,
    error: { name: error.name, message: error.message } }));
  process.exitCode = 1;
}
