// Shared real-TS recorder. The canonical generator binds the source tree
// before invoking this function. No expected Markdown is hand-authored.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, lstatSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { cases, parserGaps, fetchHtml } from './cases.mjs';

export async function record(baseline: string, sourceCommit: string, sourceSha256: Record<string, string>) {
  const locale = new Intl.Collator().resolvedOptions().locale;
  assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
  const source = (name: string) => pathToFileURL(join(baseline, 'src', name)).href;
  const { getURLMarkdownContent, __setWebFetchHookForTesting } = await import(source('tool/builtin/web/urlFetcher.ts'));
  const { WebFetchUrlCache, clearWebFetchCache } = await import(source('tool/builtin/web/urlContentCache.ts'));
  const { createWebFetchTool } = await import(source('tool/builtin/webFetch.ts'));
  const { normalizeToolError } = await import(source('tool/protocol/errors.ts'));
  const hook = (html: string) => async () => ({
    status: 200, statusText: 'OK', headers: { 'content-type': 'text/html; charset=utf-8' },
    arrayBuffer: async () => Uint8Array.from(Buffer.from(html)).buffer,
  });
  const runCases = async (inputs: typeof cases) => {
    const results = [];
    for (const spec of inputs) {
      const result = await getURLMarkdownContent('https://example.com/page', new AbortController().signal,
        { fetchHook: hook(spec.html), cache: new WebFetchUrlCache() });
      assert.ok('content' in result);
      results.push({ ...spec, markdown: result.content });
    }
    return results;
  };
  const recorded = await runCases(cases);
  const gaps = await runCases(parserGaps);
  assert.equal(recorded.length, 108);
  assert.equal(gaps.length, 69);
  assert.equal(new Set(recorded.map(c => c.name)).size, recorded.length);
  const requests: unknown[] = [];
  clearWebFetchCache();
  __setWebFetchHookForTesting(async (url: string, init: any) => {
    requests.push({ url, headers: init.headers });
    return hook(fetchHtml)();
  });
  const input = { url: 'https://example.com/page', mode: 'raw' };
  const tool = createWebFetchTool({});
  const context = { cwd: '/w', sessionId: 's', env: {}, abortSignal: new AbortController().signal };
  const results = [];
  try {
    for (let i = 0; i < 2; i++) {
      try {
        const out = await tool.execute(input, context);
        results.push({ ok: { content: out.content, data: out.data, metadata: out.metadata } });
      } catch (error) {
        const normalized = normalizeToolError(error);
        results.push({ error: { code: normalized.code, message: normalized.message, details: normalized.details } });
      }
    }
  } finally {
    __setWebFetchHookForTesting(null);
    clearWebFetchCache();
  }
  assert.equal(requests.length, 1, 'second fetch must use the cache');
  const dependencies: Record<string, unknown> = {};
  const require = createRequire(realpathSync(join(baseline, 'node_modules/turndown/lib/turndown.cjs.js')));
  for (const name of ['turndown', '@mixmark-io/domino']) {
    const directory = dirname(require.resolve(`${name}/package.json`));
    const files: Record<string, string> = {};
    const work = ['lib', 'package.json'];
    while (work.length) {
      const file = work.pop()!;
      const absolute = join(directory, file);
      if (lstatSync(absolute).isDirectory()) {
        work.push(...readdirSync(absolute).sort().map(child => `${file}/${child}`));
      } else {
        files[file] = createHash('sha256').update(readFileSync(absolute)).digest('hex');
      }
    }
    dependencies[name] = {
      version: JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')).version,
      sha256: Object.fromEntries(Object.entries(files).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)),
    };
  }
  return {
    sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8, locale, dependencies,
    cases: recorded, parserGaps: gaps, fetch: { html: fetchHtml, input, requests, results },
  };
}
