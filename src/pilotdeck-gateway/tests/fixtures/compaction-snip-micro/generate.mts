// A6.6-pr7 model-free compaction fixture. Every expected message list, count
// and byte value below comes from the fixed baseline's real SnipEngine /
// MicroCompactionEngine (context/compaction/*.ts); this generator imports them
// through the pinned tsx loader instead of re-implementing any behaviour.
//
// Run from the repo root with Node 22 and the baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstat, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const tree = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);

// Every relative import reachable from an entry is part of the calibrating
// implementation, so sourceSha256 covers the closure rather than one file.
async function resolveSource(absolute: string): Promise<string | undefined> {
  const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
  const candidates = [
    absolute,
    `${absolute}.ts`,
    `${absolute}.tsx`,
    `${absolute}.mts`,
    join(absolute, 'index.ts'),
    ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : []),
  ];
  for (const candidate of candidates) {
    try {
      if ((await lstat(candidate)).isFile()) return candidate;
    } catch {
      // try the next extension
    }
  }
  return undefined;
}
async function sourceClosure(entry: string): Promise<string[]> {
  const seen = new Set<string>([entry]);
  const queue = [entry];
  while (queue.length > 0) {
    const relativePath = queue.shift()!;
    const text = await readFile(join(baseline, relativePath), 'utf8');
    const specifiers = [
      ...text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g),
    ].map((match) => match[2]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
      const resolved = await resolveSource(join(baseline, dirname(relativePath), specifier));
      assert.ok(resolved, `unresolved baseline import ${specifier} in ${relativePath}`);
      const path = relative(baseline, resolved).split(sep).join('/');
      if (!seen.has(path)) {
        seen.add(path);
        queue.push(path);
      }
    }
  }
  return [...seen].sort();
}
const SOURCE_ENTRIES = [
  'src/context/compaction/SnipEngine.ts',
  'src/context/compaction/MicroCompactionEngine.ts',
  'src/context/compaction/toolPairIntegrity.ts',
  'src/context/compaction/protectedContext.ts',
  'src/context/budget/TokenBudgetManager.ts',
  'src/context/budget/tokenizer.ts',
  'src/model/protocol/toolResultContent.ts',
];
const closure = [...new Set((await Promise.all(SOURCE_ENTRIES.map(sourceClosure))).flat())].sort();
const sourceSha256: Record<string, string> = {};
for (const path of closure) {
  assert.ok(path.startsWith('src/'), `import closure escaped src/: ${path}`);
  const working = await readFile(join(baseline, path));
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}; the baseline checkout is not clean`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { SnipEngine } = await load('src/context/compaction/SnipEngine.ts');
const { MicroCompactionEngine } = await load('src/context/compaction/MicroCompactionEngine.ts');
const { TokenBudgetManager } = await load('src/context/budget/TokenBudgetManager.ts');

// localeCompare / tokenizer behaviour follow the ICU default locale; Rust pins
// the same one for its collation-sensitive paths. This fixture only uses
// ASCII case and UTF-16 ordering, but the assertion keeps generator runs honest.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

// ------------------------------------------------------------ input helpers
const t = (text: string) => ({ type: 'text', text });
const user = (text: string) => ({ role: 'user', content: [t(text)] });
const assistant = (text: string) => ({ role: 'assistant', content: [t(text)] });
const synthetic = (text: string) => ({ role: 'user', content: [t(text)], metadata: { synthetic: true } });
const call = (id: string, name: string, input: unknown = {}) => ({ type: 'tool_call', id, name, input });
const result = (id: string, text: string) => ({ type: 'tool_result', toolCallId: id, content: [t(text)] });
const resultBlocks = (id: string, blocks: unknown[]) => ({ type: 'tool_result', toolCallId: id, content: blocks });
const toolTurn = (n: number, id: string, tool: string, output: string) => [
  user(`request ${n}`),
  { role: 'assistant', content: [t(`calling ${tool}`), call(id, tool, { q: `q${n}` })] },
  { role: 'user', content: [result(id, output)] },
  assistant(`answer ${n}`),
];
const turns = (n: number, end = 'assistant') => Array.from({ length: n }, (_, index) =>
  end === 'assistant'
    ? [user(`request ${index + 1}`), assistant(`answer ${index + 1}`)]
    : [user(`request ${index + 1}`)],
).flat();
const flat = (parts: any[][]) => parts.flat();
const tokens = (messages: any[]) => new TokenBudgetManager().estimateMessagesTokens(messages);
const longText = (label: string) => `${label} ` + Array.from({ length: 500 }, (_, index) => `token${index}-${'abc'.repeat(4)}`).join(' ');

// JSON has no NaN/Infinity. Record those option values as `{$: 'NaN'}` (the
// same tag shape as the config-changes/session-overrides fixtures); the Rust
// replay decodes the tag in `optional_f64`.
function jsonSafe(value: any): any {
  if (typeof value === 'number') {
    if (Number.isNaN(value)) return { $: 'NaN' };
    if (value === Infinity) return { $: 'Infinity' };
    if (value === -Infinity) return { $: '-Infinity' };
    return value;
  }
  if (Array.isArray(value)) return value.map(jsonSafe);
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, jsonSafe(child)]));
  }
  return value;
}

const scenarios: any[] = [];
function snip(name: string, input: any[], options: any = {}, callOptions: any = {}) {
  const engine = new SnipEngine(options);
  const result = engine.snip(input, callOptions);
  scenarios.push({ name, engine: 'snip', options: jsonSafe(options), callOptions: jsonSafe(callOptions), input, result });
  return result;
}
function micro(name: string, input: any[], options: any = {}, inputOptions: any = {}) {
  const engine = new MicroCompactionEngine(options);
  const result = engine.apply({ messages: input, ...inputOptions });
  scenarios.push({ name, engine: 'micro', options: jsonSafe(options), inputOptions: jsonSafe(inputOptions), input, result });
  return result;
}

// ---------------------------------------------------------------- snip cases
{
  // Fidelity note: the baseline only breaks the tail loop when a token target
  // is supplied. Without one it keeps every turn from the end, so the nominal
  // keepHeadTurns/keepTailTurns defaults are a no-op (S3 short-circuit and the
  // unconstrained tail walk behave differently from the doc comment). The
  // fixture pins the real behaviour rather than the comment.
  const input = turns(8);
  const result = snip('unconstrained-head-tail-noop', input);
  assert.equal(result.applied, false, 'negative control: unconstrained 8 turns stay untouched');
  assert.equal(result.turnsSnipped, 0);
  assert.deepEqual(result.messages, input);
}
{
  // Negative control: at or below head+tail the engine must not inject a
  // boundary or reformat anything.
  const input = turns(6);
  const result = snip('noop-within-window', input);
  assert.equal(result.applied, false, 'negative control: 6 turns <= 2+4 stays untouched');
  assert.equal(result.turnsSnipped, 0);
  assert.deepEqual(result.messages, input);
}
{
  const input = turns(8);
  const result = snip('constrained-tail-window', input, {}, { targetTokens: 1 });
  assert.equal(result.applied, true);
  assert.ok(result.turnsSnipped > 0, 'negative control: the constrained tail prunes turns');
  assert.ok(result.messages.length < input.length, 'negative control: the result got shorter');
}
{
  const input = turns(8);
  const result = snip('disabled', input, { enabled: false }, { targetTokens: 1 });
  assert.equal(result.applied, false);
  assert.deepEqual(result.messages, input);
}
snip('constrained-custom-head-tail', turns(10), { keepHeadTurns: 1, keepTailTurns: 2 }, { targetTokens: 1 });
{
  const boundary = { role: 'user', content: [t('<compact-boundary turns="1" />')] };
  const summary = { role: 'assistant', content: [t('[CONTEXT COMPACTION - REFERENCE ONLY] earlier summary')] };
  const input = [boundary, summary, ...turns(8)];
  const result = snip('checkpoint-prefix', input, {}, { targetTotalTokens: tokens([boundary, summary]) + 1 });
  assert.equal(result.applied, true);
  assert.deepEqual(result.messages.slice(0, 2), [boundary, summary], 'negative control: checkpoint pair is preserved verbatim');
}
{
  const input = flat([toolTurn(1, 'c1', 'read_file', 'A'.repeat(40)), toolTurn(2, 'c2', 'bash', 'B'.repeat(40)), toolTurn(3, 'c3', 'grep', 'C'.repeat(40)), toolTurn(4, 'c4', 'glob', 'D'.repeat(40)), toolTurn(5, 'c5', 'web_search', 'E'.repeat(40))]);
  snip('tool-cycles-default', input, { keepHeadTurns: 1, keepTailTurns: 2 }, { targetTokens: 1 });
}
{
  const input = flat([
    toolTurn(1, 'c1', 'read_file', 'A'.repeat(80)),
    toolTurn(2, 'c2', 'bash', 'B'.repeat(80)),
    toolTurn(3, 'c3', 'grep', 'C'.repeat(80)),
    toolTurn(4, 'c4', 'glob', 'D'.repeat(80)),
    toolTurn(5, 'c5', 'web_search', 'E'.repeat(80)),
  ]);
  const targetTokens = Math.floor(tokens(input) * 0.35);
  snip('constrained-target-tokens', input, {}, { targetTokens });
}
{
  const boundary = { role: 'user', content: [t('<compact-boundary turns="1" />')] };
  const summary = { role: 'assistant', content: [t('[CONTEXT COMPACTION - REFERENCE ONLY] earlier summary')] };
  const input = flat([
    toolTurn(1, 'c1', 'read_file', 'A'.repeat(80)),
    toolTurn(2, 'c2', 'bash', 'B'.repeat(80)),
    toolTurn(3, 'c3', 'grep', 'C'.repeat(80)),
    toolTurn(4, 'c4', 'glob', 'D'.repeat(80)),
    toolTurn(5, 'c5', 'web_search', 'E'.repeat(80)),
  ]);
  const targetTotalTokens = Math.floor(tokens([boundary, summary]) + tokens(input) * 0.3);
  snip('constrained-total-with-checkpoint', [boundary, summary, ...input], {}, { targetTotalTokens });
}
{
  // Middle turn holds a default-protected tool result; the tail token budget
  // is small enough to prune it unless the protected-turn rule keeps it.
  const input = flat([
    toolTurn(1, 'c1', 'read_file', 'A'.repeat(60)),
    toolTurn(2, 'c2', 'bash', 'B'.repeat(60)),
    [user('please use the skill'), { ...assistant('skill'), content: [call('p1', 'read_skill')] }, { role: 'user', content: [result('p1', 'skill body')] }, assistant('skill answered')],
    toolTurn(3, 'c3', 'grep', 'C'.repeat(60)),
    toolTurn(4, 'c4', 'glob', 'D'.repeat(60)),
  ]);
  const before = snip('protected-default-tools', input, {}, { targetTokens: 180 });
  assert.ok(before.messages.some((message: any) => JSON.stringify(message).includes('skill body')), 'negative control: protected result survives');
}
{
  const input = flat([
    toolTurn(1, 'c1', 'read_file', 'A'.repeat(40)),
    [user('memory turn'), { role: 'user', content: [t('<memory-context>\nremember this\n</memory-context>'), t('request memory')] }, assistant('memory answer')],
    toolTurn(2, 'c2', 'bash', 'B'.repeat(40)),
    toolTurn(3, 'c3', 'grep', 'C'.repeat(40)),
  ]);
  snip('protected-memory-context', input, { keepHeadTurns: 1, keepTailTurns: 1 }, { targetTokens: 1 });
}
{
  const input = flat([
    toolTurn(1, 'c1', 'read_file', 'A'.repeat(40)),
    [user('custom tool turn'), { ...assistant('custom'), content: [call('x1', 'my_tool')] }, { role: 'user', content: [result('x1', 'custom result')] }, assistant('custom done')],
    toolTurn(2, 'c2', 'bash', 'B'.repeat(40)),
    toolTurn(3, 'c3', 'grep', 'C'.repeat(40)),
  ]);
  snip('protected-custom-names', input, { keepHeadTurns: 1, keepTailTurns: 1, protectedToolNames: ['my_tool'] }, { targetTokens: 1 });
}
{
  // A protected kept group has a tool call whose result lived in a snipped
  // group; the kept anchor also holds a result whose call lived in a snipped
  // group. Both dangling sides must disappear and d1 must be reported.
  const input = [
    user('head request'),
    { role: 'assistant', content: [call('p1', 'read_skill'), call('d1', 'read_file')] },
    user('interrupt'),
    { role: 'user', content: [result('d1', 'late result')] },
    { role: 'assistant', content: [call('d2', 'read_file')] },
    { role: 'user', content: [t('tail request'), result('d2', 'orphan result')] },
    assistant('tail answer'),
  ];
  const snipped = snip('dangling-tool-pairs', input, { keepHeadTurns: 1, keepTailTurns: 1 }, { targetTokens: 1 });
  assert.ok(snipped.danglingToolCallIds.includes('d1'), 'negative control: dangling d1 is reported');
  assert.ok(!JSON.stringify(snipped.messages).includes('orphan result'), 'negative control: orphan d2 result is removed');
}
{
  const input = [user('keep'), assistant('head answer'), user('middle'), assistant('middle answer'), user('tail'), assistant('tail assistant')];
  snip('trailing-assistant', input, { keepHeadTurns: 1, keepTailTurns: 1 }, { targetTokens: 1 });
}
{
  const input = [synthetic('synthetic request'), assistant('synthetic answer'), ...turns(4)];
  snip('synthetic-anchor', input, { keepHeadTurns: 1, keepTailTurns: 1 }, { targetTokens: 1 });
}
{
  const media = { role: 'user', content: [{ type: 'media_reference', toolCallId: 'm1', path: 'a.png', originalBytes: 10, preview: 'a', hasMore: false, mimeType: 'image/png', mediaType: 'image' }] };
  const input = [
    user('head'), assistant('head call'), { role: 'assistant', content: [call('m1', 'read_file')] }, media, assistant('after media'),
    user('middle'), assistant('middle answer'),
    user('tail'), assistant('tail answer'),
  ];
  snip('media-reference-tool-result', input, { keepHeadTurns: 1, keepTailTurns: 1 }, { targetTokens: 1 });
}

// --------------------------------------------------------------- micro cases
{
  const input = flat([1, 2].map((n) => [user(`request ${n}`), { ...assistant('call'), content: [call(`m${n}`, 'read_file')] }, { role: 'user', content: [result(`m${n}`, `output ${n}`)] }]));
  const output = micro('few-results-skip', input);
  assert.equal(output.rewritten, 0, 'negative control: below keepLatest stays verbatim');
  assert.deepEqual(output.messages, input);
}
{
  const input = flat(Array.from({ length: 6 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`m${index + 1}`, 'read_file')] },
    { role: 'user', content: [result(`m${index + 1}`, longText(`OUTPUT-${index + 1}`))] },
  ]));
  const output = micro('rewrite-oldest-keep-two', input, { keepLatest: 2 });
  assert.equal(output.rewritten, 4);
  assert.deepEqual(output.toolCallIds, ['m1', 'm2', 'm3', 'm4']);
}
{
  const input = flat(Array.from({ length: 5 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`n${index + 1}`, 'custom_tool')] },
    { role: 'user', content: [result(`n${index + 1}`, longText(`NON-COMPACTABLE-${index + 1}`))] },
  ]));
  const output = micro('non-compactable-names', input, { keepLatest: 1 });
  assert.equal(output.rewritten, 0, 'negative control: unknown tools are untouched');
}
{
  const input = flat([
    [user('bash request'), { ...assistant('call'), content: [call('p1', 'bash')] }, { role: 'user', content: [result('p1', longText('PROTECTED'))] }],
    ...Array.from({ length: 4 }, (_, index) => [
      user(`request ${index + 1}`),
      { ...assistant('call'), content: [call(`r${index + 1}`, 'read_file')] },
      { role: 'user', content: [result(`r${index + 1}`, longText(`OUTPUT-${index + 1}`))] },
    ]),
  ]);
  const output = micro('protected-tool-result', input, { keepLatest: 1, protectedToolNames: ['bash'] });
  assert.ok(!output.toolCallIds.includes('p1'), 'negative control: protected result is not rewritten');
}
{
  const input = flat([
    [user('bash request'), { ...assistant('call'), content: [call('p1', 'bash')] }, { role: 'user', content: [result('p1', longText('PROTECTED'))] }],
    ...Array.from({ length: 4 }, (_, index) => [
      user(`request ${index + 1}`),
      { ...assistant('call'), content: [call(`r${index + 1}`, 'read_file')] },
      { role: 'user', content: [result(`r${index + 1}`, longText(`OUTPUT-${index + 1}`))] },
    ]),
  ]);
  const output = micro('protected-override-null', input, { keepLatest: 1, protectedToolNames: ['bash'] }, { protectedToolNames: null });
  assert.ok(output.toolCallIds.includes('p1'));
}
{
  const input = flat(Array.from({ length: 5 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`t${index + 1}`, 'bash')] },
    { role: 'user', content: [result(`t${index + 1}`, longText(`TOKEN-BUDGET-${index + 1}`))] },
  ]));
  micro('trim-to-tokens', input, { keepLatest: 1, trimToTokens: 128 });
  micro('trim-to-bytes', input, { keepLatest: 1, trimToBytes: 300 });
}
{
  const input = flat([
    [user('image request'), { role: 'user', content: [{ type: 'image', source: 'base64', data: 'A'.repeat(120), mimeType: 'image/png' }] }],
    [user('pdf request'), { role: 'user', content: [{ type: 'pdf', data: 'B'.repeat(90), bytes: 90, mimeType: 'application/pdf', pages: 2 }] }],
    ...Array.from({ length: 4 }, (_, index) => [
      user(`request ${index + 1}`),
      { ...assistant('call'), content: [call(`i${index + 1}`, 'read_file')] },
      { role: 'user', content: [resultBlocks(`i${index + 1}`, [t(longText(`TEXT-${index + 1}`)), { type: 'image', source: 'base64', data: 'C'.repeat(50), mimeType: 'image/png' }])] },
    ]),
  ]);
  micro('image-pdf-clear-and-size', input, { keepLatest: 1 });
}
{
  const input = flat(Array.from({ length: 5 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`u${index + 1}`, 'web_fetch')] },
    { role: 'user', content: [result(`u${index + 1}`, `汉字🙂-${index + 1}-` + '日本語テスト'.repeat(60))] },
  ]));
  micro('unicode-token-slicing', input, { keepLatest: 1, trimToTokens: 96 });
}
{
  const input = flat(Array.from({ length: 4 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`e${index + 1}`, 'edit_file')] },
    { role: 'user', content: [result(`e${index + 1}`, longText(`EDIT-${index + 1}`))] },
  ]));
  const once = micro('idempotent-second-pass', input, { keepLatest: 1, trimToTokens: 200 });
  const twice = new MicroCompactionEngine({ keepLatest: 1, trimToTokens: 200 }).apply({ messages: once.messages });
  assert.equal(twice.rewritten, 0, 'negative control: a produced preview is not rewritten again');
  scenarios.push({ name: 'idempotent-second-pass-result', engine: 'micro', options: { keepLatest: 1, trimToTokens: 200 }, inputOptions: {}, input: once.messages, result: twice });
}
{
  const input = flat(Array.from({ length: 4 }, (_, index) => [
    user(`request ${index + 1}`),
    { ...assistant('call'), content: [call(`k${index + 1}`, 'write_file')] },
    { role: 'user', content: [result(`k${index + 1}`, longText(`WRITE-${index + 1}`))] },
  ]));
  const normal = new MicroCompactionEngine({ keepLatest: 1, trimToTokens: 768 }).apply({ messages: input });
  const emergency = new MicroCompactionEngine({ keepLatest: 1, trimToTokens: 256 }).apply({ messages: normal.messages });
  scenarios.push({ name: 'emergency-tighten-preview', engine: 'micro', options: { keepLatest: 1, trimToTokens: 768 }, inputOptions: {}, input, result: normal });
  scenarios.push({ name: 'emergency-tighten-preview-result', engine: 'micro', options: { keepLatest: 1, trimToTokens: 256 }, inputOptions: {}, input: normal.messages, result: emergency });
}
{
  // P2-2: `toolResultIndices[length - keepLatest]` is undefined for 0, a
  // negative number and a fractional number, so `undefined - 1` is NaN and
  // `index > NaN` is false: every compactable result falls in the rewrite
  // window. The Rust `rewrite_until: None` branch must match that exactly.
  const cases = [
    ['keep-latest-zero', 0],
    ['keep-latest-negative', -1],
    ['keep-latest-fractional', 1.5],
  ] as const;
  for (const [name, keepLatest] of cases) {
    const input = flat(Array.from({ length: 5 }, (_, index) => [
      user(`request ${index + 1}`),
      { ...assistant('call'), content: [call(`z${index + 1}`, 'read_file')] },
      { role: 'user', content: [result(`z${index + 1}`, longText(`NUMERIC-${keepLatest}-${index + 1}`))] },
    ]));
    const output = micro(name, input, { keepLatest });
    assert.equal(output.rewritten, 5, `negative control: keepLatest ${keepLatest} rewrites every result`);
  }
}
{
  // P2-2: a tool_result image with `source: 'url'` must flatten to
  // `[Image: <mime>, <url>]`, not the base64-character count. The URL is long
  // enough that the preview path rewrites it and keeps the URL visible.
  const url = `https://example.com/assets/${'x'.repeat(120)}.png`;
  const input = flat(Array.from({ length: 2 }, (_, index) => [
    user(`url request ${index + 1}`),
    { ...assistant('call'), content: [call(`w${index + 1}`, 'read_file')] },
    { role: 'user', content: [resultBlocks(`w${index + 1}`, [{ type: 'image', source: 'url', data: url, mimeType: 'image/png' }])] },
  ]));
  const output = micro('tool-result-image-url', input, { keepLatest: 0, trimToTokens: 24 });
  assert.equal(output.rewritten, 2, 'negative control: both url images are rewritten');
  assert.ok(JSON.stringify(output.messages).includes(url), 'negative control: the preview keeps the url rather than a base64 count');
}
{
  // P1-1: TS's stripUnpairedToolCalls ends with `.filter((m) => m.content.length > 0)`
  // over every role, so a retained `content: []` message disappears even though
  // no block was filtered in this pass. The tail group is kept by the tail floor.
  const input = [...turns(8), { role: 'assistant', content: [] }];
  const result = snip('empty-assistant-content-dropped', input, {}, { targetTokens: 1 });
  assert.equal(result.applied, true);
  assert.ok(input.some((message) => message.content.length === 0), 'input has an empty assistant message');
  assert.ok(!result.messages.some((message) => message.content.length === 0), 'negative control: empty assistant content is dropped');
}
{
  // P1-1 for stripUnpairedToolResults: the same trailing `.filter` removes a
  // user message that was already empty before this pass.
  const input = [...turns(8), { role: 'user', content: [] }];
  const result = snip('empty-user-content-dropped', input, {}, { targetTokens: 1 });
  assert.equal(result.applied, true);
  assert.ok(input.some((message) => message.content.length === 0), 'input has an empty user message');
  assert.ok(!result.messages.some((message) => message.content.length === 0), 'negative control: empty user content is dropped');
}
{
  // P2-2: `Math.max(0, NaN)` stays NaN. With a constrained target the
  // boundary text carries the unmodified `headTurns="NaN"`, pinning the
  // option path and JS number formatting. (The unconstrained head-loop
  // `Math.min(NaN, len)` is followed by a tail walk that keeps every turn,
  // so it is not separately observable.)
  const result = snip('keep-head-turns-nan', turns(8), { keepHeadTurns: NaN }, { targetTokens: 1 });
  assert.equal(result.applied, true);
  assert.ok(JSON.stringify(result.messages).includes('headTurns=\\"NaN\\"'), 'negative control: NaN is rendered, not coerced');
}

// ------------------------------------------------------------ probes (RESULT)
const probes = {
  locale,
  tokenCounts: {
    ascii: new TokenBudgetManager().estimateTextTokens('hello world'),
    cjk: new TokenBudgetManager().estimateTextTokens('汉字'),
    emoji: new TokenBudgetManager().estimateTextTokens('🙂'),
  },
  negativeControls: {
    snipSixTurnsApplied: new SnipEngine().snip(turns(6)).applied,
    microTwoResultsRewritten: new MicroCompactionEngine().apply({
      messages: flat([1, 2].map((n) => [user(`request ${n}`), { ...assistant('call'), content: [call(`m${n}`, 'read_file')] }, { role: 'user', content: [result(`m${n}`, `output ${n}`)] }])),
    }).rewritten,
  },
  boundaryText: new SnipEngine().snip(turns(8)).messages.find((message: any) =>
    message.role === 'user' && message.content.some((block: any) => block.type === 'text' && block.text.startsWith('<snip-boundary')),
  ),
};

// ----------------------------------------------------------- emit the oracle
const oracle: Record<string, any> = {
  sourceCommit: BASELINE_COMMIT,
  sourceSha256,
  baselineGuard: { ref: tree.ref, count: tree.count, sha256: tree.sha256 },
  locale,
  probes,
  scenarios,
};
const serialized = `${JSON.stringify(oracle, null, 2)}\n`;

const scratch = await mkdtemp(join(output, '.generate-'));
/** Deletes stay confined to this run's own direct `.generate-*` directory. */
function assertOwnedScratch(path: string): string {
  const rel = relative(output, path);
  if (!rel.startsWith('.generate-') || rel.includes(sep) || isAbsolute(rel)) {
    throw new Error(`refused to remove ${path}: not this run's mkdtemp tree under ${output}`);
  }
  return path;
}
try {
  assertOwnedScratch(scratch);
  const target = join(output, 'oracle.json');
  // Write inside the per-run mkdtemp directory (same filesystem), so two
  // generators writing this fixture concurrently cannot share a temp name.
  const temporary = join(scratch, 'oracle.json.tmp');
  await writeFile(temporary, serialized);
  await rename(temporary, target);
} finally {
  await rm(assertOwnedScratch(scratch), { recursive: true, force: true });
}

console.log(`wrote ${scenarios.length} scenarios, ${Object.keys(sourceSha256).length} source hashes`);
