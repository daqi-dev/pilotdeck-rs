// Only generation rules and losslessly packed real TS output enter the fixture.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a fresh output directory');
const base = resolve(process.env.PDRS_BASELINE);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(base, ref, git);
const sourceSha256 = sourceClosureSha256(base, ref, git,
  ['src/tool/builtin/webFetch.ts', 'src/tool/builtin/webSearch.ts']);
// Bind the same external parser/converter bytes as the original HTML corpus.
const original = JSON.parse(readFileSync(new URL('./oracle.json', import.meta.url), 'utf8'));
const require = createRequire(realpathSync(join(base, 'node_modules/turndown/lib/turndown.cjs.js')));
for (const [name, spec] of Object.entries(original.dependencies) as [string, any][]) {
  const directory = dirname(require.resolve(`${name}/package.json`));
  assert.equal(JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')).version, spec.version);
  for (const [file, hash] of Object.entries(spec.sha256)) {
    assert.equal(createHash('sha256').update(readFileSync(join(directory, file))).digest('hex'), hash, `${name}/${file}`);
  }
}
const { getURLMarkdownContent } = await import(pathToFileURL(join(base, 'src/tool/builtin/web/urlFetcher.ts')).href);
const { WebFetchUrlCache } = await import(pathToFileURL(join(base, 'src/tool/builtin/web/urlContentCache.ts')).href);

type Rule = string | { repeat: number, parts: Rule[] };
const repeat = (count: number, ...parts: Rule[]): Rule => ({ repeat: count, parts });
const expand = (rule: Rule): string => typeof rule === 'string' ? rule : rule.parts.map(expand).join('').repeat(rule.repeat);
const sha256 = (s: string) => createHash('sha256').update(s).digest('hex');
// Periods only choose a lossless encoding; every packed result is expanded and
// byte-compared to actual TS output, so no Markdown expectation is invented.
function pack(text: string, periods: number[]): Rule {
  if (!periods.length || !text.length) return text;
  const [period, ...rest] = periods;
  const unit = text.slice(0, period);
  let count = 0;
  while (text.startsWith(unit, count * period) && (count + 1) * period <= text.length) count++;
  if (count < 2) return pack(text, rest);
  return repeat(1, repeat(count, pack(unit, rest)), pack(text.slice(count * period), rest));
}
const denseBlock = repeat(1, '<p>', repeat(1024, '<em>x</em>'), '</p>');
const denseBlocks = Math.floor((10 * 1024 * 1024) / expand(denseBlock).length);
// Misnested formatting repeatedly reparents a wide P subtree. At 2 MiB
// domino succeeds; its larger-input array-limit gap is disclosed separately.
const formatting = 240;
const widePrefix = Array.from({ length: formatting }, (_, i) => `<b a=${i}>`).join('') + '<p>';
const wideSuffix = '</b>'.repeat(formatting);
const wideBytes = 2 * 1024 * 1024;
const wideCount = Math.floor((wideBytes - widePrefix.length - wideSuffix.length) / '<span>x</span>'.length);
const widePadding = wideBytes - widePrefix.length - wideSuffix.length - wideCount * '<span>x</span>'.length;
const specs = [
  { name: 'table-4-columns-30000-rows', html: repeat(1, '<table>', repeat(30000, '<tr>', repeat(4, '<td>cell</td>'), '</tr>'), '</table>'), periods: [6] },
  { name: 'paragraphs-100000', html: repeat(100000, '<p>paragraph</p>'), periods: [11] },
  // Shallow dense inline siblings in 1024-tag paragraphs: full 10 MiB remains
  // practical for TS, whose repeated string joins otherwise scale poorly.
  { name: 'dense-inline-10mib-grouped', html: repeat(1, repeat(denseBlocks, denseBlock), 'x'.repeat(10 * 1024 * 1024 - denseBlocks * expand(denseBlock).length)), periods: [3074, 3, 1] },
  // A single wide inline run independently covers the old node rejection.
  { name: 'dense-inline-50000', html: repeat(50000, '<em>x</em>'), periods: [3] },
  { name: 'reparent-wide-2mib', html: repeat(1, widePrefix, repeat(wideCount, '<span>x</span>'), 'x'.repeat(widePadding), wideSuffix), periods: [2, 1] },
];
const cases = [];
for (const spec of specs) {
  const html = expand(spec.html);
  const started = performance.now();
  const value = await getURLMarkdownContent('https://example.com/page', new AbortController().signal, {
    fetchHook: async () => ({ status: 200, statusText: 'OK', headers: { 'content-type': 'text/html' }, arrayBuffer: async () => Buffer.from(html) }),
    cache: new WebFetchUrlCache(),
  });
  assert.ok('content' in value);
  const markdown = pack(value.content, spec.periods);
  assert.ok(Buffer.from(expand(markdown)).equals(Buffer.from(value.content)), 'packed TS bytes');
  const record = { name: spec.name, html: spec.html, htmlBytes: Buffer.byteLength(html), htmlSha256: sha256(html), markdown, markdownBytes: Buffer.byteLength(value.content), markdownSha256: sha256(value.content) };
  assert.ok(JSON.stringify(record).length < 16000, 'large input/output must stay generation rules');
  cases.push(record);
  console.log(JSON.stringify({ name: spec.name, htmlBytes: record.htmlBytes, markdownBytes: record.markdownBytes, tsMs: performance.now() - started, markdownSha256: record.markdownSha256 }));
}
writeFileSync(join(resolve(process.argv[2]), 'large-oracle.json'), JSON.stringify({
  sourceCommit: ref, sourceSha256, node: process.version, v8: process.versions.v8,
  dependencies: original.dependencies, cases,
}, null, 1) + '\n', { flag: 'wx' });
