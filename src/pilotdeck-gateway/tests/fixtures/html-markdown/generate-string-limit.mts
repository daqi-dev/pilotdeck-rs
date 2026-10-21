// Real web_fetch observations: the fetch hook supplies bytes without a socket.
import assert from 'node:assert/strict';
import { writeFileSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
const base = resolve(process.env.PDRS_BASELINE!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(base, ref, git);
const sourceSha256 = sourceClosureSha256(base, ref, git, ['src/tool/builtin/webFetch.ts']);
const source = (p: string) => pathToFileURL(join(base, 'src', p)).href;
const { getURLMarkdownContent, __setWebFetchHookForTesting } = await import(source('tool/builtin/web/urlFetcher.ts'));
const { WebFetchUrlCache, clearWebFetchCache } = await import(source('tool/builtin/web/urlContentCache.ts'));
const { createWebFetchTool } = await import(source('tool/builtin/webFetch.ts'));
const { normalizeToolError } = await import(source('tool/protocol/errors.ts'));
const input = { url: 'https://example.com/page', mode: 'raw' };
const context = { cwd: '/w', sessionId: 's', env: {}, abortSignal: new AbortController().signal };
let requests = 0;
let html = '';
const hook = async () => {
  requests++;
  return { status: 200, statusText: 'OK', headers: { 'content-type': 'text/html' }, arrayBuffer: async () => Buffer.from(html) };
};
const cases = [];
try {
  __setWebFetchHookForTesting(hook);
  // n=16 records 655349 literal Markdown bytes (<1 MiB).
  for (const n of [16, 33]) {
    html = '<h1><div>'.repeat(n) + 'x' + '</div></h1>'.repeat(n);
    const cache = new WebFetchUrlCache();
    let direct: { markdown: string } | { error: { name: string; message: string } };
    try {
      const out = await getURLMarkdownContent(input.url, context.abortSignal, { fetchHook: hook, cache });
      assert.ok('content' in out);
      direct = { markdown: out.content };
    } catch (error) {
      assert.ok(error instanceof Error);
      direct = { error: { name: error.name, message: error.message } };
    }
    const cachedAfterDirect = cache.get(input.url) !== undefined;
    clearWebFetchCache();
    requests = 0;
    const results = [];
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const out = await createWebFetchTool({}).execute(input, context);
        // Keep error records exact; success is already recorded as raw Markdown.
        results.push({ fromCache: out.data?.fromCache, rawLength: out.data?.rawLength });
      } catch (error) {
        const e = normalizeToolError(error);
        results.push({ error: { code: e.code, message: e.message, details: e.details } });
      }
    }
    cases.push({ n, html, direct, cachedAfterDirect, requests, results });
  }
} finally {
  __setWebFetchHookForTesting(null);
  clearWebFetchCache();
}
writeFileSync(join(resolve(process.argv[2]), 'string-limit-oracle.json'), JSON.stringify({
  sourceCommit: ref, sourceSha256, node: process.version, v8: process.versions.v8, cases,
}, null, 1) + '\n', { flag: 'wx' });
console.log(JSON.stringify(cases.map(({ n, direct, ...rest }) => ({ n, ...rest, direct: 'markdown' in direct ? { markdownLength: direct.markdown.length } : direct })), null, 2));
