import { baselineSourceHashes } from '../_shared/baseline.mts';
// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { lstatSync, realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;

// Pin the process clock so the runtime's `now`/TTL and any Date use is fixed.
const RealDate = Date;
class FixedDate extends RealDate {
  constructor(...args: any[]) {
    super(...((args.length === 0 ? [0] : args) as []));
  }
  static now(): number {
    return 0;
  }
}
globalThis.Date = FixedDate as unknown as DateConstructor;

// Import-closure hashing: every relative import reached from the two entries.
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
      if (lstatSync(candidate).isFile()) return candidate;
    } catch {
      // try the next extension
    }
  }
  return undefined;
}
async function sourceClosure(entry: string, seen: Set<string>): Promise<void> {
  const queue = [entry];
  while (queue.length > 0) {
    const relativePath = queue.shift()!;
    const text = await readFile(join(baseline, relativePath), 'utf8');
    const specifiers = [...text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g)].map(m => m[2]);
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
}
const closure = new Set<string>();
await sourceClosure('src/router/RouterRuntime.ts', closure);
await sourceClosure('src/pilot/config/loadPilotConfig.ts', closure);
const sourceSha256 = await baselineSourceHashes(baseline, [...closure, 'src/model/index.ts']);

const load = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const { createRouterRuntime } = await load('router/RouterRuntime.ts');
const { loadPilotConfig } = await load('pilot/config/loadPilotConfig.ts');
const { ModelProviderError, ModelRequestError } = await load('model/index.ts');

const PROVIDERS = `model:
  providers:
    p1:
      protocol: openai
      url: http://127.0.0.1:1/v1
      apiKey: mock-only
      models:
        m1: {capabilities: {maxOutputTokens: 100}}
        m2: {capabilities: {maxOutputTokens: 50}}
    p2:
      protocol: openai
      url: http://127.0.0.1:2/v1
      apiKey: mock-only
      models:
        m1: {capabilities: {maxOutputTokens: 100}}
        m2: {capabilities: {maxOutputTokens: 200}}
    p3:
      protocol: openai
      url: http://127.0.0.1:3/v1
      apiKey: mock-only
      models:
        m1: {capabilities: {maxOutputTokens: 100}}
        m3: {capabilities: {maxOutputTokens: 300}}
`;
const yaml = (router: string) => `schemaVersion: 1
agent: {model: p1/m1}
${PROVIDERS}router:
${router.trimEnd().split('\n').map(line => `  ${line}`).join('\n')}
`;

const text = (value: string) => [{ type: 'text', text: value }];
const msg = (role: string, content: any[], metadata?: any) => ({ role, content, ...(metadata ? { metadata } : {}) });
const request = (messages: any[], extra: any = {}) => ({ provider: 'p1', model: 'm1', messages, ...extra });
const user = (value: string) => msg('user', text(value));

const cases: any[] = [];
function add(item: any) {
  cases.push(item);
}
const baseCaps = { 'p1/m1': 100, 'p1/m2': 50, 'p2/m1': 100, 'p2/m2': 200, 'p3/m1': 100, 'p3/m3': 300 };

// 1. Master switch: decisions are pure passthrough, including subagent shape.
add({
  id: 'disabled-router',
  yaml: yaml(`enabled: false
scenarios: {default: p1/m1}`),
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: false },
  ],
});

// 2. Scenario resolution: default, subagent flag, tag hint, explicit metadata.
add({
  id: 'scenario-resolution',
  yaml: yaml(`scenarios: {default: p1/m1}`),
  steps: [
    { kind: 'decide', request: request([user('read this')], { tools: [{ name: 'read_file', description: 'r', inputSchema: {} }] }), sessionId: 's', isMainAgent: true },
    { kind: 'decide', request: request([user('do it')], { tools: [{ name: 'agent', description: 'a', inputSchema: {} }] }), sessionId: 's', isMainAgent: false },
    { kind: 'decide', request: request([user('<ccr-subagent-model>p2/m2</ccr-subagent-model>  go')]), sessionId: 's', isMainAgent: true },
    { kind: 'materialize', request: request([user('\ud800  <ccr-subagent-model>p2/m2</ccr-subagent-model>  tail  ')]) },
    { kind: 'decide', request: request([user('hi')]), sessionId: 's', isMainAgent: false, metadata: { explicitProvider: 'p3', explicitModel: 'm3' } },
  ],
});

// 2b. ES non-unicode `i` never folds non-ASCII onto ASCII. U+017F LONG S must
//     not pass for `s` in the agent-tool name or the subagent tag.
add({
  id: 'unicode-case-fold',
  yaml: yaml(`scenarios: {default: p1/m1}`),
  steps: [
    { kind: 'decide', request: request([user('hello')], { tools: [{ name: 'ta\u017Fk', description: 'a', inputSchema: {} }] }), sessionId: 'long-s-tool', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')], { tools: [{ name: 'task', description: 'a', inputSchema: {} }] }), sessionId: 'ascii-tool', isMainAgent: true },
    { kind: 'decide', request: request([user('<ccr-\u017Fubagent-model>p2/m2</ccr-\u017Fubagent-model>')], { tools: [{ name: 'task', description: 'a', inputSchema: {} }] }), sessionId: 'long-s-tag', isMainAgent: true },
    { kind: 'decide', request: request([user('<ccr-subagent-model>p2/m2</ccr-subagent-model>  tail')], { tools: [{ name: 'task', description: 'a', inputSchema: {} }] }), sessionId: 'ascii-tag', isMainAgent: true },
    { kind: 'materialize', request: request([user('  <ccr-\u017Fubagent-model>p2/m2</ccr-\u017Fubagent-model>  tail  ')]) },
    { kind: 'materialize', request: request([user('  <ccr-subagent-model>p2/m2</ccr-subagent-model>  tail  ')]) },
  ],
});

// 2c. U+212A KELVIN SIGN must not match `[a-z]`; the ASCII tier still parses.
//     `k` is a real tier so pre-fix Rust (Unicode `(?i)` + toLowerCase) would
//     resolve the judge output to `k` while TS falls back to the default tier.
add({
  id: 'unicode-tier-tag',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: complex
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
    k: {model: p1/m2}`),
  judgeScript: [
    { text: '<tier>\u212A</tier>' },
    { text: '<tier>\u212A</tier>' },
    { text: '<tier>\u212A</tier>' },
    { text: '<tier>SIMPLE</tier>' },
  ],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 'kelvin', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'ascii', isMainAgent: true },
  ],
});

// 2d. JS `\b` is a word/non-word transition, not "both neighbours are
//     non-word". A tier whose first or last code unit is not an ASCII word unit
//     (CJK, trailing `-`, Greek τ) only matches when the adjacent code unit is a
//     word unit; the pre-fix Rust check matched in the opposite cases. Values
//     below are the real TS parseTier output (node probe):
//       use 简单 mode → default fallback; x简单y → 简单
//       use beta- now → default;        beta-x → beta-
//       use τ now     → default;        xτy     → τ
//       use simple now → simple (ASCII control)
add({
  id: 'tier-word-boundaries',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: fallback
  tiers:
    简单: {model: p1/m1}
    "beta-": {model: p1/m2}
    τ: {model: p3/m1}
    simple: {model: p3/m3}
    fallback: {model: p2/m2}`),
  judgeScript: [
    { text: 'use 简单 mode' },
    { text: 'use 简单 mode' },
    { text: 'use 简单 mode' },
    { text: 'x简单y' },
    { text: 'use beta- now' },
    { text: 'use beta- now' },
    { text: 'use beta- now' },
    { text: 'beta-x' },
    { text: 'use τ now' },
    { text: 'use τ now' },
    { text: 'use τ now' },
    { text: 'xτy' },
    { text: 'use simple now' },
  ],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 'cjk-bare', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'cjk-word-adjacent', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'dash-bare', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'dash-word-adjacent', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'tau-bare', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'tau-word-adjacent', isMainAgent: true },
    { kind: 'decide', request: request([user('hello')]), sessionId: 'ascii-control', isMainAgent: true },
  ],
});

// 3. Sticky: the second main turn with history reuses the first judge result.
add({
  id: 'sticky',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ text: '<tier>simple</tier>' }, { text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: true },
    { kind: 'invalidate', sessionId: 's' },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: true },
  ],
});

// 4. Cache-aware switching: previous provider/model + observed cache hits keep
//    the cheaper sticky model instead of the judge's target.
add({
  id: 'cache-aware',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
  cacheAwareSwitching: {enabled: true, minSavingsRatio: 0}
stats:
  enabled: true
  modelPricing:
    p1/m1: {input: 10, output: 30, cacheRead: 0.1}
    p3/m3: {input: 100, output: 300, cacheRead: 1}`),
  judgeScript: [{ text: '<tier>simple</tier>' }, { text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'observeUsage', sessionId: 's', usage: { inputTokens: 100, outputTokens: 10, cacheReadTokens: 80, totalTokens: 110 } },
    { kind: 'invalidate', sessionId: 's' },
    {
      kind: 'decide',
      request: request([user('hello'), msg('assistant', text('hi')), user('again')]),
      sessionId: 's',
      isMainAgent: true,
      metadata: { previousProvider: 'p1', previousModel: 'm1', previousTier: 'simple' },
    },
  ],
});

// 5. Orchestration admission: first turn activates, second continues.
add({
  id: 'orchestration',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: complex
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
autoOrchestrate:
  enabled: true
  triggerTiers: [complex]`),
  judgeScript: [{ text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('plan a big refactor')]), sessionId: 's', isMainAgent: true },
    { kind: 'decide', request: request([user('plan'), msg('assistant', text('ok')), user('continue')]), sessionId: 's', isMainAgent: true },
  ],
});

// 6. Media capability reroute to the first compatible fallback.
add({
  id: 'media-reroute',
  yaml: yaml(`scenarios: {default: p1/m1}
fallback: {default: [p2/m2]}`),
  multimodal: { 'p1/m1': ['text'], 'p2/m2': ['text', 'image'] },
  steps: [
    {
      kind: 'decide',
      request: request([msg('user', [{ type: 'text', text: 'look' }, { type: 'image', source: 'url', mimeType: 'image/png', data: 'https://example.test/a.png' }])]),
      sessionId: 's',
      isMainAgent: true,
    },
  ],
});

// 7. Non-retryable provider error: judge falls back to default tier and emits
//    the sanitized token-saver failure event.
add({
  id: 'judge-model-error',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ error: { kind: 'provider', code: 'auth_error', message: 'Authorization: Bearer sk-secret-123', retryable: false } }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 8. Timeout: min 500ms; no retry for timeouts.
add({
  id: 'judge-timeout',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
  judgeTimeoutMs: 500`),
  judgeScript: [{ hang: true }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 9. Subagent policy `skip`: no judge call at all.
add({
  id: 'subagent-skip',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
  subagent: {policy: skip}`),
  judgeScript: [{ text: '<tier>simple</tier>' }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: false }],
});

// 10. Host/SDK model policy rejection.
add({
  id: 'validate-policy',
  yaml: yaml(`scenarios: {default: p1/m1}`),
  denied: ['p2/m2'],
  steps: [
    { kind: 'validate', provider: 'p1', model: 'm1' },
    { kind: 'validate', provider: 'p2', model: 'm2' },
    { kind: 'validate', provider: 'p1', model: 'm1', policy: { allow: [], deny: ['p1/*'] } },
    { kind: 'validate', provider: 'p3', model: 'm3', policy: { allow: ['p1/*'], deny: [] } },
    { kind: 'validate', provider: 'p1', model: 'm1', policy: { allow: ['p1/m1'], deny: [] } },
  ],
});

// 11. Materialization: cache plan routing and max-output clamping.
add({
  id: 'materialize',
  yaml: yaml(`scenarios: {default: p1/m1}`),
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    {
      kind: 'materialize',
      request: request([user('hello')], {
        maxOutputTokens: 1000,
        cachePlan: { provider: 'p1', model: 'm1', messages: [0], tools: true, system: true, fingerprint: 'f', generation: 1 },
        cacheBreakpoints: [0],
      }),
    },
    {
      kind: 'materialize',
      request: request([user('hello')], {
        maxOutputTokens: 20,
        cachePlan: { provider: 'p2', model: 'm2', messages: [0], tools: false, system: false, fingerprint: 'g', generation: 2 },
        cacheBreakpoints: [0],
      }),
    },
  ],
});

// 12. Custom router selection wins over the configured scenario.
add({
  id: 'custom-router',
  yaml: yaml(`scenarios: {default: p1/m1}
customRouter: {extensionId: acme.router}`),
  custom: { provider: 'p3', model: 'm3' },
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 13. Sticky TTL: expires exactly at 1h with strict `<`; a sticky hit refreshes.
add({
  id: 'sticky-expiry',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ text: '<tier>simple</tier>' }, { text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'advanceClock', ms: 3_600_000 },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: true },
    { kind: 'advanceClock', ms: 3_600_001 },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: true },
  ],
});

// 14. A judge timeout must not mark the caller's turn-level abort signal.
add({
  id: 'judge-caller-abort',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
  judgeTimeoutMs: 500`),
  judgeScript: [{ hang: true }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true, abort: 'active' }],
});

// 15. Retryable provider error: 3 attempts, then fallback; CJK before `token`
//     still gets redacted (`\b` is ASCII-only in JS without the `u` flag).
add({
  id: 'judge-retryable-provider',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ error: { kind: 'provider', code: 'server_error', message: '\u65e0\u6548token=sk-abc', retryable: true } }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 16. ModelRequestError is not retried.
add({
  id: 'judge-request-error',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ error: { kind: 'request', code: 'invalid_request', message: 'bad token=sk-xyz' } }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 17. Empty judge output three times: parse_error fallback with attempts=3 and
//     no code/message keys.
add({
  id: 'judge-parse-error',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ empty: true }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 17b. Retry success: empty first attempt, tier on the second.
add({
  id: 'judge-retry-success',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ empty: true }, { text: '<tier>complex</tier>' }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 18. Subagent judge policy keeps a separate `:sub` sticky slot.
add({
  id: 'subagent-judge-sticky',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
  subagent: {policy: judge}`),
  judgeScript: [{ text: '<tier>simple</tier>' }, { text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: false },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: false },
    { kind: 'decide', request: request([user('hello'), msg('assistant', text('hi')), user('again')]), sessionId: 's', isMainAgent: true },
  ],
});

// 19. invalidateSticky while orchestrating keeps provider/model/tier.
add({
  id: 'invalidate-orchestrating',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: complex
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
autoOrchestrate:
  enabled: true
  triggerTiers: [complex]`),
  judgeScript: [{ text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'decide', request: request([user('plan a big refactor')]), sessionId: 's', isMainAgent: true },
    { kind: 'invalidate', sessionId: 's' },
    { kind: 'decide', request: request([user('plan'), msg('assistant', text('ok')), user('continue')]), sessionId: 's', isMainAgent: true },
  ],
});

// 20. Cache-aware `switched`: the prefill target is cheaper than the cached
//     current model.
add({
  id: 'cache-aware-switched',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}
  cacheAwareSwitching: {enabled: true, minSavingsRatio: 0}
stats:
  enabled: true
  modelPricing:
    p1/m1: {input: 100, output: 300, cacheRead: 1}
    p3/m3: {input: 1, output: 3, cacheRead: 1}`),
  judgeScript: [{ text: '<tier>complex</tier>' }],
  steps: [
    { kind: 'observeUsage', sessionId: 's', usage: { inputTokens: 100, outputTokens: 10, cacheReadTokens: 80, totalTokens: 110 } },
    {
      kind: 'decide',
      request: request([user('hello')]),
      sessionId: 's',
      isMainAgent: true,
      metadata: { previousProvider: 'p1', previousModel: 'm1', previousTier: 'simple' },
    },
  ],
});

// 21. A throwing custom router emits `pilotdeck_router_custom_failed` and falls
//     back to the scenario.
add({
  id: 'custom-failed',
  yaml: yaml(`scenarios: {default: p1/m1}
customRouter: {extensionId: acme.router}`),
  custom: { provider: 'p3', model: 'm3' },
  customError: 'router exploded',
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

// 22. Disabled router: invalidate is a no-op shape.
add({
  id: 'disabled-invalidate',
  yaml: yaml(`enabled: false
scenarios: {default: p1/m1}`),
  steps: [
    { kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true },
    { kind: 'invalidate', sessionId: 's' },
  ],
});

// 23. Empty description and empty previousTier are falsy in the judge prompt.
add({
  id: 'empty-description',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3, description: ""}`),
  judgeScript: [{ text: '<tier>simple</tier>' }],
  steps: [
    {
      kind: 'decide',
      request: request([user('hello')]),
      sessionId: 's',
      isMainAgent: true,
      metadata: { previousTier: '' },
    },
  ],
});

// 24. Truthiness of error code/message: empty strings are omitted from events.
add({
  id: 'judge-empty-error-fields',
  yaml: yaml(`scenarios: {default: p1/m1}
tokenSaver:
  enabled: true
  judge: p1/m1
  defaultTier: simple
  tiers:
    simple: {model: p1/m1}
    complex: {model: p3/m3}`),
  judgeScript: [{ error: { kind: 'provider', code: '', message: '', retryable: false } }],
  steps: [{ kind: 'decide', request: request([user('hello')]), sessionId: 's', isMainAgent: true }],
});

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-router-decision-'));
const recorded: any[] = [];
try {
  for (const item of cases) {
    const root = join(scratch, item.id);
    const home = join(root, 'home');
    await mkdir(home, { recursive: true });
    const configPath = join(home, 'pilotdeck.yaml');
    await writeFile(configPath, item.yaml);
    const snapshot = loadPilotConfig({
      env: { HOME: home, PILOT_HOME: home, PILOTDECK_SKIP_BROWSER_OPEN: '1' },
      configPath,
      version: 1,
    });
    const routerConfig = JSON.parse(JSON.stringify(snapshot.config.router));
    const capabilities = { ...baseCaps, ...(item.capabilities ?? {}) };
    const multimodal = item.multimodal ?? {};
    const judgeScript = item.judgeScript ?? [{ text: '<tier>simple</tier>' }];
    let judgeCall = 0;
    let nowMs = 0;
    const judgeRequests: string[] = [];
    const events: any[] = [];
    const fake = {
      async *stream() { throw new Error('stream is not used by decision fixtures'); },
      getCapabilities(provider: string, model: string) {
        const key = `${provider}/${model}`;
        if (!(key in capabilities)) throw new Error(`Unknown provider/model ${key}`);
        return { maxOutputTokens: capabilities[key], maxContextTokens: 200000 };
      },
      getMultimodal(provider: string, model: string) {
        return { input: multimodal[`${provider}/${model}`] ?? ['text', 'image', 'pdf', 'audio'] };
      },
      getProviderProtocol() { return 'openai'; },
      getProviderBaseUrl() { return 'http://127.0.0.1:1/v1'; },
      async complete(req: any) {
        const prompt = req.messages?.[0]?.content?.find((b: any) => b.type === 'text')?.text;
        judgeRequests.push(prompt);
        const script = judgeScript[Math.min(judgeCall, judgeScript.length - 1)];
        judgeCall += 1;
        if (script.hang) return new Promise(() => {});
        if (script.error) {
          if (script.error.kind === 'provider') {
            throw new ModelProviderError({ code: script.error.code, message: script.error.message, retryable: script.error.retryable, provider: req.provider, protocol: 'openai' } as any);
          }
          throw new ModelRequestError(script.error.code, script.error.message);
        }
        return { content: script.empty ? [] : [{ type: 'text', text: script.text }], finishReason: 'stop' };
      },
    };
    const runtime = createRouterRuntime(routerConfig, {
      modelInvoker: fake as any,
      judgeInvoker: fake as any,
      now: () => new Date(nowMs),
      events: { emit: (event: any) => events.push(JSON.parse(JSON.stringify(event))) },
      isModelAllowed: (model: any) => !(item.denied ?? []).includes(`${model.provider}/${model.model}`),
      ...(item.custom ? {
        customRouterRegistry: {
          lookupRouter: () => ({
            decide: async () => {
              if (item.customError) throw new Error(item.customError);
              return { provider: item.custom.provider, model: item.custom.model };
            },
          }),
        },
      } : {}),
    });
    const expected: any[] = [];
    let lastDecision: any;
    let eventIndex = 0;
    for (const step of item.steps) {
      const startEvents = eventIndex;
      let result: any;
      let controller: AbortController | undefined;
      if (step.kind === 'decide' && step.abort) {
        controller = new AbortController();
        if (step.abort === 'before') controller.abort();
      }
      try {
        if (step.kind === 'decide') {
          lastDecision = await runtime.decide({
            request: step.request,
            sessionId: step.sessionId,
            isMainAgent: step.isMainAgent,
            ...(step.metadata ? { metadata: step.metadata } : {}),
            ...(controller ? { abortSignal: controller.signal } : {}),
          });
          result = { decision: JSON.parse(JSON.stringify(lastDecision)) };
          if (controller) result.callerAborted = controller.signal.aborted;
        } else if (step.kind === 'invalidate') {
          result = { invalidated: JSON.parse(JSON.stringify(runtime.invalidateSticky(step.sessionId))) };
        } else if (step.kind === 'observeUsage') {
          runtime.observeUsage(step.sessionId, step.usage);
          result = { observed: true };
        } else if (step.kind === 'advanceClock') {
          nowMs += step.ms;
          result = { clock: nowMs };
        } else if (step.kind === 'validate') {
          runtime.validateModelSelection({ provider: step.provider, model: step.model }, step.policy);
          result = { ok: true };
        } else if (step.kind === 'materialize') {
          result = { materialized: JSON.parse(JSON.stringify(runtime.materializeRequest(lastDecision, step.request))) };
        } else {
          throw new Error(`unknown step ${step.kind}`);
        }
      } catch (error: any) {
        result = { error: { name: error?.name ?? 'Error', code: error?.code ?? null, message: error?.message ?? String(error) } };
      }
      eventIndex = events.length;
      expected.push({ ...result, judgeCalls: judgeCall, events: events.slice(startEvents) });
    }
    recorded.push({
      id: item.id,
      snapshot: JSON.parse(JSON.stringify({ config: { router: routerConfig, model: snapshot.config.model } })),
      capabilities,
      multimodal,
      denied: item.denied ?? [],
      custom: item.custom ?? null,
      customError: item.customError ?? null,
      judgeScript,
      steps: item.steps,
      expected,
      judgeRequests,
    });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}

const oracle = { baselineCommit: sourceCommit, sourceSha256, locale, cases: recorded };
await writeFile(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(`wrote ${recorded.length} router decision cases`);
