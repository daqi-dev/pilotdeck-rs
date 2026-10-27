// #940: real TS conversion, deterministic synchronous entry/exit and abort hooks.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.equal(process.version, 'v22.22.0', '#940 requires Node v22.22.0');
assert.ok(process.env.PDRS_BASELINE, '#940 requires PDRS_BASELINE');
assert.ok(process.argv[2], '#940 requires an output directory');
const base = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(base, sourceCommit, git);
const sourceSha256 = sourceClosureSha256(base, sourceCommit, git, ['src/tool/builtin/webFetch.ts']);
const src = (path: string) => pathToFileURL(join(base, 'src', path)).href;
const { createWebFetchTool } = await import(src('tool/builtin/webFetch.ts'));
const { __setWebFetchHookForTesting } = await import(src('tool/builtin/web/urlFetcher.ts'));
const { clearWebFetchCache } = await import(src('tool/builtin/web/urlContentCache.ts'));
const require = createRequire(join(base, 'package.json'));
const Turndown = require('turndown');
const original = Turndown.prototype.turndown;
const html = '<p>converted</p>';
const cases = [];
const deadline = setTimeout(() => { throw new Error('#940 TS probe deadline'); }, 10000);
try {
  for (const abort of [false, true]) {
    clearWebFetchCache();
    const controller = new AbortController();
    const events: string[] = [];
    __setWebFetchHookForTesting(async () => ({
      status: 200, statusText: 'OK', headers: { 'content-type': 'text/html' },
      arrayBuffer: async () => Buffer.from(html),
    }));
    Turndown.prototype.turndown = function (input: string) {
      events.push('conversion-enter');
      if (abort) { controller.abort(); events.push('parent-abort'); }
      const result = original.call(this, input);
      events.push('conversion-exit');
      return result;
    };
    const out = await createWebFetchTool({}).execute(
      { url: 'https://example.com/page', mode: 'raw' },
      { cwd: '/w', sessionId: 's', env: {}, abortSignal: controller.signal },
    );
    events.push('result');
    assert.deepEqual(events, abort
      ? ['conversion-enter', 'parent-abort', 'conversion-exit', 'result']
      : ['conversion-enter', 'conversion-exit', 'result']);
    cases.push({ abort, events, result: { ok: { content: out.content, data: out.data, metadata: out.metadata } } });
  }
} finally {
  clearTimeout(deadline);
  Turndown.prototype.turndown = original;
  __setWebFetchHookForTesting(null);
  clearWebFetchCache();
}
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, node: process.version, html, cases }, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ node: process.version, cases }, null, 2));
