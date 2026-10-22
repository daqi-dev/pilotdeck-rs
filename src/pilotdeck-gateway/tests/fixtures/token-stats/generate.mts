// Real-TypeScript oracle for the Rust `TokenStatsCollector` port.
//
// Sources are imported from the fixed baseline commit and hashed before use.
// Every expected value below (records after `observe`, aggregates, snapshots,
// projections and journal bytes) is produced by the real TS implementation;
// nothing is hand-written except the input event stream.
//
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/token-stats/generate.mts /tmp/<owned-run>
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sources = [
  'src/router/stats/TokenStatsCollector.ts',
  'src/router/utils/modelPricing.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/router/config/parseRouterConfig.ts',
  'src/router/config/schema.ts',
];
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const importBaseline = (relative: string) =>
  import(pathToFileURL(join(baseline, relative)).href);
const { TokenStatsCollector } = await importBaseline('src/router/stats/TokenStatsCollector.ts');
const { ProjectRuntimeRegistry } = await importBaseline('src/cli/ProjectRuntimeRegistry.ts');
const { parseRouterConfig } = await importBaseline('src/router/config/parseRouterConfig.ts');

const clone = (value: any) => (value === undefined ? null : JSON.parse(JSON.stringify(value)));
const json = (value: any) => JSON.stringify(value);

function listing(home: string) {
  const files: Record<string, string> = {};
  if (!existsSync(home)) return files;
  for (const name of readdirSync(home).sort()) {
    const path = join(home, name);
    files[name] = readFileSync(path, 'utf8');
  }
  return files;
}

function projectionReceiver(stats: any) {
  // Only the resolver seam is used; `usageSnapshotForSdk` /
  // `modelUsageSnapshotForSdk` are the real baseline methods.
  return { resolve: () => ({ router: { stats } }) };
}
function usageProjection(stats: any, input: any) {
  return ProjectRuntimeRegistry.prototype.usageSnapshotForSdk.call(
    projectionReceiver(stats),
    input,
  );
}
function modelUsageProjection(stats: any, input: any) {
  return ProjectRuntimeRegistry.prototype.modelUsageSnapshotForSdk.call(
    projectionReceiver(stats),
    input,
  );
}

type Scenario = {
  name: string;
  config: any;
  /** Fixed `Date.now()` for boundary scenarios; omitted means the real clock. */
  now?: number;
  initial?: Record<string, string>;
  ops: Array<{ op: string; [key: string]: any }>;
};
const scenarios: Scenario[] = [];
const models = (entries: Record<string, any>) => entries;

// ── A. multi-provider / fallback / failure / cache tokens / cost provenance ──
const CONFIG_A = {
  enabled: true,
  modelPricing: models({
    'custom/exact-model': { input: 1.25, output: 7.5, cacheRead: 0.5 },
    'sonnet-4': { input: 4, output: 20, cacheRead: 1 },
  }),
  baselineModel: { provider: 'anthropic', model: 'claude-sonnet-4-5-20250929' },
};
const event = (
  sessionId: string,
  turnId: string | undefined,
  scenarioType: string,
  resolvedFrom: string,
  provider: string,
  model: string,
  usage: any,
  startedAt: string,
  endedAt: string,
  extra: any = {},
) => {
  const record: any = { sessionId };
  if (turnId) record.turnId = turnId;
  if (extra.projectPath) record.projectPath = extra.projectPath;
  record.scenarioType = scenarioType;
  record.resolvedFrom = resolvedFrom;
  record.provider = provider;
  record.model = model;
  if (extra.tier) record.tier = extra.tier;
  if (extra.role) record.role = extra.role;
  record.usage = usage;
  record.startedAt = startedAt;
  record.endedAt = endedAt;
  return record;
};
const EVENTS_A = [
  event('s1', 't1', 'default', 'scenario', 'anthropic', 'claude-sonnet-4-5-20250929',
    { inputTokens: 1000, outputTokens: 500, cacheReadTokens: 200, cacheWriteTokens: 100, totalTokens: 1800 },
    '2026-03-01T10:00:00.000Z', '2026-03-01T10:00:05.000Z',
    { projectPath: '/pdrs/project', tier: 'medium', role: 'main' }),
  event('s1', 't1', 'default', 'fallback', 'openai', 'gpt-4o',
    { inputTokens: 1200, outputTokens: 450, cacheReadTokens: 0, totalTokens: 1650 },
    '2026-03-01T10:00:05.000Z', '2026-03-01T10:00:09.000Z',
    { projectPath: '/pdrs/project', tier: 'medium', role: 'main' }),
  event('s1', 't2', 'explicit', 'explicit', 'google', 'gemini-2.5-flash',
    { inputTokens: 10, outputTokens: 2 },
    '2026-03-01T10:05:00.000Z', '2026-03-01T10:05:01.000Z', { role: 'subagent' }),
  event('s2', 't3', 'subagent', 'tokenSaver', 'openrouter', 'openrouter/auto',
    { inputTokens: 77, outputTokens: 11, nativeCost: 0.0123 },
    '2026-03-01T23:59:59.000Z', '2026-03-01T23:59:59.500Z', { tier: 'simple', role: 'main' }),
  event('s2', 't4', 'default', 'scenario', 'custom', 'exact-model',
    { inputTokens: 2000000, outputTokens: 100000, cacheReadTokens: 500000, cacheWriteTokens: 250000 },
    '2026-03-02T00:00:00.000Z', '2026-03-02T00:00:02.000Z', { role: 'main' }),
  event('s2', 't5', 'default', 'scenario', 'unknown', 'unknown-model', {},
    '2026-03-02T00:00:02.000Z', '2026-03-02T00:00:03.000Z', { role: 'main' }),
  event('s1', 't6', 'default', 'tokenSaver', 'anthropic', 'claude-sonnet-4-5-20250929',
    { inputTokens: 0, outputTokens: 0, totalTokens: 0 },
    '2026-03-02T01:00:00.000Z', '2026-03-02T01:00:01.000Z', { tier: 'simple', role: 'main' }),
  event('s3', 't7', 'default', 'custom', 'meta', 'llama-3-70b',
    { inputTokens: 5, outputTokens: 3, cacheReadTokens: 1, cacheWriteTokens: 2, totalTokens: 11 },
    '2026-03-02T01:00:00.000Z', '2026-03-02T01:00:01.000Z', { role: 'subagent' }),
  // Deliberately observed last but started earlier: `recent()` sorts by
  // startedAt before slicing, so this is the newest item only by insertion.
  event('s4', 't8', 'default', 'scenario', 'anthropic', 'claude-sonnet-4-5-20250929',
    { inputTokens: 9, outputTokens: 4 },
    '2026-02-28T23:00:00.000Z', '2026-02-28T23:00:01.000Z', { role: 'main' }),
];
{
  const ops: Array<{ op: string; [key: string]: any }> = [];
  for (const record of EVENTS_A) {
    ops.push({ op: 'observe', record });
    ops.push({ op: 'snapshot' });
  }
  ops.push(
    { op: 'files' },
    { op: 'hourly' },
    { op: 'session', sessionId: 's1' },
    { op: 'modelUsage' },
    { op: 'modelUsage', sessionId: 's1' },
    { op: 'estimateCost', usage: { inputTokens: 77, outputTokens: 11, nativeCost: 0.0123 }, provider: 'openrouter', model: 'openrouter/auto' },
    { op: 'estimateCost', usage: { inputTokens: 2000000, outputTokens: 100000 }, provider: 'custom', model: 'exact-model' },
    { op: 'estimateCost', usage: { inputTokens: 2000000, outputTokens: 100000, nativeCost: 0 }, provider: 'custom', model: 'exact-model' },
    { op: 'estimateCost', provider: 'custom', model: 'exact-model' },
    { op: 'recent', limit: 3 },
    { op: 'usageProjection', sessionKey: 's1' },
    { op: 'modelUsageProjection', sessionKey: 's1' },
    { op: 'usageProjection' },
    { op: 'modelUsageProjection' },
    { op: 'files' },
    { op: 'reopen' },
    { op: 'snapshot' },
    { op: 'hourly' },
    { op: 'recent', limit: 3 },
    { op: 'usageProjection', sessionKey: 's2' },
    { op: 'files' },
  );
  scenarios.push({ name: 'aggregate-and-projection', config: CONFIG_A, ops });
}

// ── B. hourly/session/recent windows and pruning ─────────────────────────────
{
  const ops: Array<{ op: string; [key: string]: any }> = [];
  const base = Date.parse('2026-01-01T00:00:00.000Z');
  for (let index = 0; index < 505; index += 1) {
    const hour = index % 80;
    const started = new Date(base + hour * 3_600_000 + index * 1000).toISOString();
    const ended = new Date(base + hour * 3_600_000 + index * 1000 + 500).toISOString();
    ops.push({
      op: 'observe',
      record: event(
        `w${index % 205}`,
        `t${index}`,
        'default',
        'scenario',
        index % 2 === 0 ? 'anthropic' : 'openai',
        index % 2 === 0 ? 'claude-sonnet-4-5-20250929' : 'gpt-4o',
        { inputTokens: index, outputTokens: index % 7, cacheReadTokens: index % 3 },
        started,
        ended,
        { role: index % 4 === 0 ? 'subagent' : 'main' },
      ),
    });
  }
  ops.push(
    { op: 'snapshot' },
    { op: 'session', sessionId: 'w115' },
    { op: 'session', sessionId: 'w69' },
    { op: 'files' },
    { op: 'hourly' },
    { op: 'recent', limit: 600 },
    { op: 'modelUsage' },
    { op: 'reopen' },
    { op: 'snapshot' },
    { op: 'hourly' },
    { op: 'recent', limit: 600 },
    { op: 'modelUsage' },
    { op: 'files' },
  );
  scenarios.push({ name: 'windows-and-pruning', config: { enabled: true }, ops });
}

// ── C. retention: expired rows dropped, compacted journal, reopened ──────────
const RETAINED_ROW = {
  sessionId: 'keep',
  scenarioType: 'default',
  resolvedFrom: 'scenario',
  provider: 'anthropic',
  model: 'claude-sonnet-4-5-20250929',
  usage: { inputTokens: 7, outputTokens: 3 },
  startedAt: '2999-01-01T00:00:00.000Z',
  endedAt: '2999-01-01T00:00:01.000Z',
  cost: { input: 0.000028, output: 0.000045, cacheRead: 0, total: 0.000073 },
  costSource: 'built_in_estimate',
  baselineCost: 0.000073,
};
const EXPIRED_ROW = {
  ...RETAINED_ROW,
  sessionId: 'expired',
  startedAt: '2000-01-01T00:00:00.000Z',
  endedAt: '2000-01-01T00:00:01.000Z',
};
const MALFORMED_ROW = {
  ...RETAINED_ROW,
  sessionId: 'malformed',
  endedAt: 'not-a-date',
};
// Rows the retention decision can reach. V8 parses a wide legacy grammar; the
// Rust port deliberately interprets only the canonical `toISOString()` shapes
// and keeps everything else (disclosed in
// `specs/gateway-parity/accepted-diffs.md` A6.4-STATS-RETENTION-001). Rows
// whose V8 parse is a finite *past* instant are therefore absent here: TS
// deletes them, Rust keeps them, and that divergence is pinned by Rust unit
// tests instead. Rows below are canonical, NaN in V8, or parse to a future
// instant (`future-hour-24`), so both implementations agree — do not move such
// a row into the past without also updating the disclosure.
const OLD = (sessionId: string, endedAt: string) => ({
  ...RETAINED_ROW,
  sessionId,
  startedAt: '2001-01-01T00:00:00.000Z',
  endedAt,
});
const RETENTION_ROWS = [
  EXPIRED_ROW,
  RETAINED_ROW,
  MALFORMED_ROW,
  OLD('bad-offset-hour', '2000-01-01T00:00:00+99:99'),
  OLD('bad-offset-minute', '2000-01-01T00:00:00+01:99'),
  OLD('offset-24', '2000-01-01T00:00:00+24:00'),
  OLD('bad-day-32', '2000-01-32T00:00:00Z'),
  // codex r2 inputs: all NaN in V8, so both sides keep them. `+汉a` used to
  // panic the old hand-written offset parser; the rest slipped through its
  // grammar and were deleted as old rows.
  OLD('non-ascii-offset', '2000-01-01T00:00:00+汉a'),
  OLD('fraction-without-seconds', '2000-01-01T00:00.500Z'),
  OLD('hour-24-with-sub-ms', '2000-01-01T24:00:00.0001Z'),
  OLD('six-digit-unsigned-year', '002000-01-01T00:00:00Z'),
  // Canonical timestamps: millisecond precision and a real leap day.
  OLD('leap-day-ms', '2024-02-29T12:34:56.789Z'),
  OLD('canonical-ms', '2999-01-02T00:00:00.001Z'),
  OLD('future-hour-24', '2999-01-01T24:00:00.000Z'),
];
{
  const initial = { 'stats.jsonl': RETENTION_ROWS.map(json).join('\n') + '\n' };
  scenarios.push({
    name: 'retention-compaction',
    config: { enabled: true, retentionMs: 60_000 },
    initial,
    ops: [
      { op: 'files' },
      { op: 'snapshot' },
      { op: 'hourly' },
      { op: 'modelUsage' },
      { op: 'reopen' },
      { op: 'snapshot' },
      {
        op: 'observe',
        record: event('fresh', 't1', 'default', 'scenario', 'openai', 'gpt-4o',
          { inputTokens: 1, outputTokens: 1 }, '2999-01-02T00:00:00.000Z', '2999-01-02T00:00:01.000Z'),
      },
      { op: 'snapshot' },
      { op: 'files' },
    ],
  });
}

// ── C2. retention boundary: fixed clock, exact `now - retentionMs` ──────────
// `isRetained` compares `endedAt >= now - retentionMs`; a row exactly on the
// boundary must survive. `Date.now` is pinned so the generator records the
// canonical millisecond timestamps around it.
{
  const now = Date.UTC(2026, 2, 1, 12, 0, 0, 500);
  const boundary = (sessionId: string, endedAt: string) => ({
    ...RETAINED_ROW,
    sessionId,
    startedAt: endedAt,
    endedAt,
  });
  const rows = [
    boundary('boundary-equal', new Date(now - 60_000).toISOString()),
    boundary('boundary-inside', new Date(now - 59_999).toISOString()),
    boundary('boundary-outside', new Date(now - 60_001).toISOString()),
    boundary('leap-day-old', '2024-02-29T12:34:56.789Z'),
  ];
  scenarios.push({
    name: 'retention-boundary',
    now,
    config: { enabled: true, retentionMs: 60_000 },
    initial: { 'stats.jsonl': rows.map(json).join('\n') + '\n' },
    ops: [{ op: 'files' }, { op: 'snapshot' }, { op: 'reopen' }, { op: 'files' }],
  });
}

// ── C3. expanded-year canonical timestamps and the `Date` valid range ───────
// `toISOString()` writes `±YYYYYY-MM-DDTHH:mm:ss.sssZ` outside 0000-9999.
// `now` is pinned just past the last valid instant, with zero retention, so
// every parsed canonical timestamp expires while every NaN (out-of-range or
// non-canonical) row is kept.
{
  const now = 8_640_000_000_000_002;
  const expanded = (sessionId: string, endedAt: string) => ({
    ...RETAINED_ROW,
    sessionId,
    startedAt: endedAt,
    endedAt,
  });
  const rows = [
    expanded('negative-year', '-000001-12-31T23:59:59.999Z'),
    expanded('negative-expanded', '-010000-01-01T00:00:00.000Z'),
    expanded('positive-expanded', '+010000-01-01T00:00:00.000Z'),
    expanded('max-date', '+275760-09-13T00:00:00.000Z'),
    expanded('min-date', '-271821-04-20T00:00:00.000Z'),
    // Outside the valid `Date` range: V8 `Date.parse` is NaN, so both keep.
    expanded('above-max', '+275760-09-13T00:00:00.001Z'),
    expanded('below-min', '-271821-04-19T23:59:59.999Z'),
    expanded('above-max-year', '+999999-12-31T23:59:59.999Z'),
    expanded('below-min-year', '-999999-12-31T23:59:59.999Z'),
    // Not canonical expanded years, and NaN in V8: both keep.
    expanded('negative-zero', '-000000-01-01T00:00:00.000Z'),
    expanded('unsigned-expanded', '010000-01-01T00:00:00.000Z'),
  ];
  scenarios.push({
    name: 'retention-expanded-years',
    now,
    config: { enabled: true, retentionMs: 0 },
    initial: { 'stats.jsonl': rows.map(json).join('\n') + '\n' },
    ops: [{ op: 'files' }, { op: 'snapshot' }, { op: 'reopen' }, { op: 'files' }],
  });
}

// ── G. per-session request-log cap: >200 keeps the newest 100 ────────────────
{
  const ops: Array<{ op: string; [key: string]: any }> = [];
  const base = Date.parse('2026-05-01T00:00:00.000Z');
  for (let index = 0; index < 201; index += 1) {
    ops.push({
      op: 'observe',
      record: event(
        'cap',
        `t${index}`,
        'default',
        'scenario',
        'openai',
        'gpt-4o',
        { inputTokens: 1, outputTokens: 1 },
        new Date(base + index * 1_000).toISOString(),
        new Date(base + index * 1_000 + 500).toISOString(),
      ),
    });
    if (index >= 198) ops.push({ op: 'session', sessionId: 'cap' });
  }
  ops.push(
    { op: 'snapshot' },
    { op: 'session', sessionId: 'cap' },
    { op: 'reopen' },
    { op: 'session', sessionId: 'cap' },
    { op: 'snapshot' },
    { op: 'files' },
  );
  scenarios.push({ name: 'session-request-log-cap', config: { enabled: true }, ops });
}

// ── D. one-time stats.json -> stats.jsonl migration ──────────────────────────
{
  const legacy = {
    sessions: {
      old: {
        requestLog: [
          { ...RETAINED_ROW, sessionId: 'old', startedAt: '2026-04-02T00:00:00.000Z', endedAt: '2026-04-02T00:00:01.000Z' },
          { ...RETAINED_ROW, sessionId: 'old', startedAt: '2026-04-01T00:00:00.000Z', endedAt: '2026-04-01T00:00:01.000Z' },
        ],
      },
    },
  };
  scenarios.push({
    name: 'json-migration',
    config: { enabled: true },
    initial: { 'stats.json': json(legacy) + '\n' },
    ops: [
      { op: 'files' },
      { op: 'snapshot' },
      { op: 'reopen' },
      { op: 'snapshot' },
      { op: 'observe', record: event('after', 't1', 'default', 'scenario', 'openai', 'gpt-4o',
        { inputTokens: 2, outputTokens: 1 }, '2026-04-03T00:00:00.000Z', '2026-04-03T00:00:01.000Z') },
      { op: 'snapshot' },
      { op: 'files' },
    ],
  });
}

// ── E. disabled collector writes nothing and returns empty snapshots ─────────
{
  scenarios.push({
    name: 'disabled',
    config: { enabled: false },
    ops: [
      { op: 'observe', record: EVENTS_A[0] },
      { op: 'snapshot' },
      { op: 'hourly' },
      { op: 'modelUsage' },
      { op: 'recent', limit: 5 },
      { op: 'usageProjection' },
      { op: 'files' },
    ],
  });
}

// ── F. clear() truncates the journal and resets aggregates ───────────────────
{
  scenarios.push({
    name: 'clear',
    config: { enabled: true },
    ops: [
      { op: 'observe', record: EVENTS_A[0] },
      { op: 'observe', record: EVENTS_A[3] },
      { op: 'snapshot' },
      { op: 'clear' },
      { op: 'snapshot' },
      { op: 'files' },
      { op: 'reopen' },
      { op: 'snapshot' },
      { op: 'files' },
    ],
  });
}

// ── H. stats config projection (baselineModel / pricingNumber / retention) ───
const CONFIG_MODELS = {
  providers: { anthropic: { models: { 'claude-sonnet-4-5-20250929': {} } } },
};
const CONFIG_CASES = [
  { name: 'string-baseline', raw: { stats: { baselineModel: 'anthropic/claude-sonnet-4-5-20250929' } } },
  { name: 'object-trim', raw: { stats: { baselineModel: { provider: ' anthropic ', model: ' claude-sonnet-4-5-20250929 ' } } } },
  { name: 'object-empty', raw: { stats: { baselineModel: { provider: '  ', model: 'x' } } } },
  { name: 'string-no-slash', raw: { stats: { baselineModel: 'anthropic' } } },
  {
    name: 'pricing-values',
    raw: {
      stats: {
        modelPricing: {
          'anthropic/claude-sonnet-4-5-20250929': { input: -1, output: 2, cacheRead: 'x' },
        },
      },
    },
  },
  { name: 'retention-safe-integer', raw: { stats: { retentionMs: 1e18 } } },
  { name: 'retention-valid', raw: { stats: { retentionMs: 60_000 } } },
  { name: 'baseline-unknown-provider', raw: { stats: { baselineModel: 'missing/model' } } },
  { name: 'baseline-unknown-model', raw: { stats: { baselineModel: 'anthropic/missing' } } },
  { name: 'baseline-object-unknown-provider', raw: { stats: { baselineModel: { provider: ' missing ', model: ' model ' } } } },
  { name: 'baseline-object-unknown-model', raw: { stats: { baselineModel: { provider: ' anthropic ', model: ' missing ' } } } },
  {
    name: 'pricing-mixed-refs',
    raw: { stats: { modelPricing: {
      'missing/model': { input: -1 }, // Unknown ref skips rate diagnostics.
      'anthropic/claude-sonnet-4-5-20250929': { input: 1, output: 2, cacheRead: 0 },
      'anthropic/missing': { output: 3 },
    }, baselineModel: 'missing/baseline' } },
  },
  { name: 'pricing-no-valid-refs', raw: { stats: { modelPricing: {
    'missing/model': { input: 1 }, 'anthropic/missing': { output: 2 },
  } } } },
  { name: 'pricing-malformed-refs', raw: { stats: { modelPricing: {
    '': {}, 'anthropic': {}, '/model': {}, 'anthropic/': {},
  } } } },

];
const configCases = CONFIG_CASES.map((item) => {
  const parsed = parseRouterConfig(item.raw, CONFIG_MODELS as any);
  return {
    name: item.name,
    raw: item.raw,
    modelConfig: CONFIG_MODELS,
    stats: clone(parsed.config?.stats ?? null),
    diagnostics: clone(parsed.diagnostics),
  };
});

mkdirSync(output, { recursive: true });
const recorded: any[] = [];
for (const scenario of scenarios) {
  const home = join(output, scenario.name);
  mkdirSync(home, { recursive: true });
  for (const [name, contents] of Object.entries(scenario.initial ?? {})) {
    writeFileSync(join(home, name), contents);
  }
  const config = {
    ...scenario.config,
    ...(scenario.config.enabled ? { filePath: join(home, 'stats.jsonl') } : {}),
  };
  // `TokenStatsCollector` reads `Date.now()` in `isRetained`; pin it for the
  // boundary scenario so the exact `now - retentionMs` row stays reproducible.
  const realNow = Date.now;
  if (typeof scenario.now === 'number') {
    Date.now = () => scenario.now as number;
  }
  try {
    let stats = new TokenStatsCollector(config);
    const steps: any[] = [];
    for (const step of scenario.ops) {
      let result: any = null;
      switch (step.op) {
        case 'observe': {
          const observed = clone(step.record);
          stats.observe(observed);
          result = clone(observed);
          break;
        }
        case 'snapshot':
          result = clone(stats.snapshot());
          break;
        case 'hourly':
          result = clone(stats.hourlySnapshots());
          break;
        case 'session':
          result = clone(stats.sessionSnapshot?.(step.sessionId) ?? null);
          break;
        case 'modelUsage':
          result = clone(stats.modelUsageSnapshot?.(step.sessionId));
          break;
        case 'recent':
          result = clone(stats.recent(step.limit));
          break;
        case 'estimateCost':
          result = stats.estimateCost(step.usage, step.provider, step.model);
          break;
        case 'usageProjection':
          result = clone(usageProjection(stats, step.sessionKey ? { sessionKey: step.sessionKey } : {}));
          break;
        case 'modelUsageProjection':
          result = clone(modelUsageProjection(stats, step.sessionKey ? { sessionKey: step.sessionKey } : {}));
          break;
        case 'reopen':
          stats.dispose();
          stats = new TokenStatsCollector(config);
          break;
        case 'clear':
          stats.clear();
          break;
        case 'files':
          break;
        default:
          throw new Error(`unknown op ${step.op}`);
      }
      steps.push({
        ...step,
        expected: { result, ...(step.op === 'files' ? { files: listing(home) } : {}) },
      });
    }
    stats.dispose();
    recorded.push({
      name: scenario.name,
      ...(typeof scenario.now === 'number' ? { now: scenario.now } : {}),
      config: scenario.config,
      initial: scenario.initial ?? {},
      steps,
    });
  } finally {
    Date.now = realNow;
  }
}

writeFileSync(
  join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, scenarios: recorded, configCases }, null, 2) + '\n',
  { flag: 'wx' },
);
