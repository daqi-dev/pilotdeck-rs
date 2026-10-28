// Real-TS truncation oracle. Inputs are stored as repeat recipes and expanded
// before generation; expectations are the actual TS tool results, never
// hand-written Markdown. The bounded Rust path must reproduce them exactly.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a fresh output directory');
const base = resolve(process.env.PDRS_BASELINE);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindBaseline({ baseline: base, commit: ref, gitExecutable: git });
const sourceSha256 = sourceClosureSha256(base, ref, git,
  ['src/tool/builtin/webFetch.ts', 'src/tool/builtin/web/urlFetcher.ts']);
const original = JSON.parse(readFileSync(new URL('./oracle.json', import.meta.url), 'utf8'));
const require = createRequire(realpathSync(join(base, 'node_modules/turndown/lib/turndown.cjs.js')));
for (const [name, spec] of Object.entries(original.dependencies) as [string, any][]) {
  const directory = dirname(require.resolve(`${name}/package.json`));
  assert.equal(JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')).version, spec.version);
  for (const [file, hash] of Object.entries(spec.sha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(directory, file))).digest('hex'), hash, `${name}/${file}`);
  }
}
const source = (name: string) => pathToFileURL(join(base, 'src', name)).href;
const { getURLMarkdownContent, __setWebFetchHookForTesting } = await import(source('tool/builtin/web/urlFetcher.ts'));
const { WebFetchUrlCache, clearWebFetchCache } = await import(source('tool/builtin/web/urlContentCache.ts'));
const { createWebFetchTool } = await import(source('tool/builtin/webFetch.ts'));
const { normalizeToolError } = await import(source('tool/protocol/errors.ts'));

type Segment = { text: string, repeat: number };
const segments = (...parts: [string, number][]): Segment[] =>
  parts.map(([text, repeat]) => ({ text, repeat }));
const expand = (parts: Segment[]) => parts.map(({ text, repeat }) => text.repeat(repeat)).join('');
const sha256 = (value: Buffer) => createHash('sha256').update(value).digest('hex');
// Buffer.from(str, 'utf16le') preserves lone surrogates, unlike utf8.
const shaUtf16 = (text: string) => sha256(Buffer.from(text, 'utf16le'));
const response = (html: string) => async () => ({
  status: 200,
  statusText: 'OK',
  headers: { 'content-type': 'text/html; charset=utf-8' },
  arrayBuffer: async () => Uint8Array.from(Buffer.from(html)).buffer,
});

// Boundaries chosen around MAX_MARKDOWN_LENGTH = 100000 UTF-16 units:
// ASCII/BMP/surrogate cuts, a dangling Markdown escape, blockquote line
// prefixes, a CJK h1 underline, a wide page, trailing-space trimming, and an
// all-whitespace result whose rawLength is zero after Turndown's trim.
const specs: { name: string, parts: Segment[] }[] = [
  { name: 'under-99999', parts: segments(['<p>', 1], ['a', 99999], ['</p>', 1]) },
  { name: 'exact-100000', parts: segments(['<p>', 1], ['a', 100000], ['</p>', 1]) },
  { name: 'over-by-one-100001', parts: segments(['<p>', 1], ['a', 100001], ['</p>', 1]) },
  { name: 'cjk-cut-at-100000', parts: segments(['<p>', 1], ['a', 99999], ['中', 10], ['</p>', 1]) },
  { name: 'surrogate-split-at-100000', parts: segments(['<p>', 1], ['a', 99999], ['😀', 1], ['b', 10], ['</p>', 1]) },
  { name: 'escape-split-at-100000', parts: segments(['<p>', 1], ['a', 99999], ['*', 1], ['b', 10], ['</p>', 1]) },
  { name: 'blockquote-250-prefix-lines', parts: segments(['<blockquote>', 250], ['a<br>', 300], ['</blockquote>', 250]) },
  { name: 'cjk-h1-underline', parts: segments(['<h1>', 1], ['中', 60000], ['</h1>', 1]) },
  { name: 'wide-paragraph-page', parts: segments(['<p>paragraph</p>', 12000]) },
  { name: 'trailing-space-trim', parts: segments(['<p>', 1], ['a', 100500], [' ', 2000], ['</p>', 1]) },
  { name: 'all-whitespace-br', parts: segments(['<br>', 40000]) },
  // codex r1 P1-1: a whitespace-only content with no newline keeps its
  // whitespace-ness when an indent prefix is not inserted (real TS ">").
  { name: 'nbsp-blockquote-empty-img', parts: segments(['<blockquote>', 1], ['&nbsp;', 1], ['<img src="">', 1], ['</blockquote>', 1]) },
  { name: 'nbsp-nested-blockquote-empty-link', parts: segments(['<blockquote>', 2], ['&nbsp;', 1], ['<a href=""></a>', 1], ['</blockquote>', 2]) },
  // codex r1 P1-2: `<code>` and an inline flank must not flatten the whole
  // nested-heading subtree before the final truncation.
  { name: 'code-wrapped-nested-headings-n16', parts: segments(['<code>', 1], ['<h1><div>', 16], ['x', 1], ['</div></h1>', 16], ['</code>', 1]) },
  { name: 'span-space-code-nested-headings-n16', parts: segments(['<span> ', 1], ['<code>', 1], ['<h1><div>', 16], ['x', 1], ['</div></h1>', 16], ['</code>', 1], ['</span>', 1]) },
  // fix1 codex P1-2 continuation: a huge `<span>` flank-trimmed to a prefix
  // keeps a tail backtick symbolic; `<code>` must still choose two backticks.
  { name: 'trimmed-span-code-tail-backtick', parts: segments(['<code>', 1], ['<span>', 1], ['<h1><div>', 16], ['x', 1], ['</div></h1>', 16], ['`', 1], [' </span>', 1], ['x', 1], ['</code>', 1]) },
  // A following sibling backtick exercises the merge boundary: dropping the
  // materialized `PartialText` must not swallow the sibling.
  { name: 'trimmed-span-code-tail-backtick-sibling', parts: segments(['<code>', 1], ['<span>', 1], ['<h1><div>', 16], ['x', 1], ['</div></h1>', 16], ['`', 1], [' </span>', 1], ['`', 1], ['</code>', 1]) },
  // gate2 boundary P1-1 at production scale: content L=99,994, delimiter
  // count=5 and pad=1 (wrapper=6). The logical span is 100,006 units but the
  // bounded materialization must still cover the closing wrapper, otherwise
  // the 100,001-unit take is short by one unit.
  { name: 'code-closing-wrapper-window', parts: segments(['<code>', 1], ['`', 1], ['a', 99982], ['``', 1], ['b', 1], ['```', 1], ['c', 1], ['````', 1], ['</code>', 1]) },
];

// Per-k windows: the same real TS content sliced at each k. `k` is the
// bounded prefix length the Rust side is asked for, so these cover cut points
// far below MAX_MARKDOWN_LENGTH where the symbolic materialization decides the
// bytes (Grok r2 P1 and the gate2 boundary P1s).
const windowSpecs: { name: string, parts: Segment[], ks: number[] }[] = [
  // grok r2 P1: flatten_span's whole-content fast path ignored replace_newlines.
  { name: 'grok-fast-path-nested-h1', parts: segments(['<code>', 1], ['<h1><div>', 2], ['a\nb', 1], ['</div></h1>', 2], ['</code>', 1]), ks: [39, 40] },
  { name: 'grok-fast-path-nested-h1-backtick', parts: segments(['<code>', 1], ['<h1><div>', 2], ['xxxxxxxxxxx`y', 1], ['</div></h1>', 2], ['</code>', 1]), ks: [80, 81, 82] },
  { name: 'grok-fast-path-br', parts: segments(['<code>', 1], ['xxxa<br>b`', 1], ['</code>', 1]), ks: [10] },
  // gate2 boundary P1-1 small window (wrapper=6, L=17, k=24): same algebra as
  // the production case above, so a regression fails without a 100 KB input.
  { name: 'code-closing-wrapper-small', parts: segments(['<code>', 1], ['`a``b```c````dddd', 1], ['</code>', 1]), ks: [24] },
  // gate2 boundary P1-2: `<pre>` keeps U+000D alive and the code sibling gets
  // the inline rule, so has_cr + a flank-trimmed TextPartial is reachable.
  { name: 'code-has-cr-span-partial', parts: segments(['<pre>x<code>', 1], ['<span> <h1><div><h1><div>y</div></h1></div></h1> </span>&#13;z', 1], ['</code></pre>', 1]), ks: [10] },
  // GLM r3 P1 == gate3 boundary P1 #1: Turndown folds `\r\n` into ONE space
  // (`content.replace(/\r?\n|\r/g, " ")`), and the pair is one UTF-16 unit.
  // `&#13;&#10;` arrives as CR then LF, so the pairing must cross node seams.
  { name: 'code-crlf-window', parts: segments(['<pre>x<code>', 1], ['aaaa&#13;&#10;bbbb', 1], ['</code></pre>', 1]), ks: [6, 7, 8] },
  // Same pair, but the LF is a literal newline in the source after the entity.
  { name: 'code-crlf-entity-literal-window', parts: segments(['<pre>x<code>', 1], ['aaaa&#13;\nbbbb', 1], ['</code></pre>', 1]), ks: [6, 7, 8] },
  // Lone CR, lone LF and CR LF (including CR LF directly followed by CR) in one
  // code span: cut points land on and after every folded match.
  { name: 'code-newline-mix-window', parts: segments(['<pre>x<code>', 1], ['a&#13;&#10;b&#13;c&#10;d&#13;&#10;&#13;e', 1], ['</code></pre>', 1]), ks: [2, 4, 8, 10] },
  // GLM r4 P1: a flank-trimmed `<span>` keeps an unmaterialized tail after its
  // materialized prefix, then a sibling text starts with LF. At k=20 the raw
  // prefix (need = k + 4 = 24) is `a` + ten CR LF pairs + `cc` + CR, i.e. it
  // ends exactly on the CR, and the span's tail sits between that CR and the
  // sibling's LF. Ten pairs also make the folded prefix (`k - 6` units) shorter
  // than the requested take, so an un-grown prefix lets the bounded walk cross
  // the seam and skip the tail; k=19/20/22/28 cover both sides.
  { name: 'code-crlf-partial-seam-window', parts: segments(['<pre>x<code>q<span> a', 1], ['&#13;&#10;', 10], ['cc&#13;', 1], ['b', 20], [' </span>&#10;z', 1], ['</code></pre>', 1]), ks: [19, 20, 22, 28] },
];

const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const url = 'https://example.com/page';
const input = { url, mode: 'raw' };
const context = { cwd: '/w', sessionId: 's', env: {}, abortSignal: new AbortController().signal };
const cases = [];
for (const spec of specs) {
  const html = expand(spec.parts);
  const direct = await getURLMarkdownContent(url, new AbortController().signal, {
    fetchHook: response(html),
    cache: new WebFetchUrlCache(),
  });
  assert.ok('content' in direct);
  const full = { utf16Length: direct.content.length, sha256: shaUtf16(direct.content) };
  // The Rust bounded path may keep one extra unit when the cut splits a
  // surrogate pair, so compare the first 100001 units (the TS slice).
  const prefix = direct.content.slice(0, 100001);
  const prefixInfo = { utf16Length: prefix.length, sha256: shaUtf16(prefix) };

  clearWebFetchCache();
  const requests: string[] = [];
  __setWebFetchHookForTesting(async (requested: string) => {
    requests.push(requested);
    return response(html)();
  });
  const results: any[] = [];
  try {
    const tool = createWebFetchTool({});
    for (let i = 0; i < 2; i++) {
      try {
        const out = await tool.execute(input as any, context as any);
        const text = out.content[0].text;
        results.push({
          fromCache: out.data.fromCache,
          rawLength: out.data.rawLength,
          truncated: out.data.truncated,
          returnedLength: out.data.returnedLength,
          textUtf16Length: text.length,
          textSha256: shaUtf16(text),
        });
      } catch (error) {
        const normalized = normalizeToolError(error);
        results.push({ error: { code: normalized.code, message: normalized.message, details: normalized.details } });
      }
    }
  } finally {
    __setWebFetchHookForTesting(null);
    clearWebFetchCache();
  }
  assert.equal(requests.length, 1, `${spec.name}: second fetch must use the cache`);
  assert.equal(results.length, 2, `${spec.name}: two tool calls`);
  assert.ok(!('error' in results[0]) && !('error' in results[1]), `${spec.name}: ${JSON.stringify(results)}`);
  assert.equal(results[1].fromCache, true, `${spec.name}: cache hit`);
  assert.equal(results[0].fromCache, false, `${spec.name}: first miss`);
  assert.equal(results[0].rawLength, full.utf16Length, `${spec.name}: rawLength`);
  assert.equal(results[0].truncated, full.utf16Length > 100000, `${spec.name}: truncated flag`);
  assert.equal(results[0].textSha256, results[1].textSha256, `${spec.name}: cache output`);
  assert.equal(results[0].returnedLength, results[0].textUtf16Length, `${spec.name}: returnedLength`);
  cases.push({
    name: spec.name,
    parts: spec.parts,
    htmlBytes: Buffer.byteLength(html),
    htmlSha256: sha256(Buffer.from(html)),
    full,
    prefix: prefixInfo,
    tool: results,
  });
  console.log(JSON.stringify({ name: spec.name, htmlBytes: Buffer.byteLength(html), full: full.utf16Length, truncated: results[0].truncated, returned: results[0].returnedLength }));
}
assert.equal(new Set(cases.map(c => c.name)).size, cases.length);
const windows = [];
for (const spec of windowSpecs) {
  const html = expand(spec.parts);
  const direct = await getURLMarkdownContent(url, new AbortController().signal, {
    fetchHook: response(html),
    cache: new WebFetchUrlCache(),
  });
  assert.ok('content' in direct);
  const full = direct.content;
  const expected = spec.ks.map((k) => {
    assert.ok(k < full.length, `${spec.name}: k=${k} must be below ${full.length}`);
    const slice = full.slice(0, k);
    return { k, utf16Length: slice.length, sha256: shaUtf16(slice) };
  });
  windows.push({
    name: spec.name,
    parts: spec.parts,
    htmlBytes: Buffer.byteLength(html),
    htmlSha256: sha256(Buffer.from(html)),
    full: { utf16Length: full.length, sha256: shaUtf16(full) },
    expected,
  });
  console.log(JSON.stringify({ window: spec.name, full: full.length, ks: spec.ks }));
}
assert.equal(new Set(windows.map(c => c.name)).size, windows.length);
writeFileSync(join(resolve(process.argv[2]), 'truncate-oracle.json'), JSON.stringify({
  sourceCommit: ref,
  sourceSha256,
  node: process.version,
  v8: process.versions.v8,
  locale,
  dependencies: original.dependencies,
  cases,
  windows,
}, null, 1) + '\n', { flag: 'wx' });
