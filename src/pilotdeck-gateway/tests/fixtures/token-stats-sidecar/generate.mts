// Run only the pinned real TS collector, never the Rust implementation.
// PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//   src/pilotdeck-gateway/tests/fixtures/token-stats-sidecar/generate.mts /tmp/owned-new-directory
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a new or empty output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/router/stats/TokenStatsCollector.ts', 'src/router/utils/modelPricing.ts',
]);
const { TokenStatsCollector } = await import(pathToFileURL(join(baseline, 'src/router/stats/TokenStatsCollector.ts')).href);
fs.mkdirSync(output, { recursive: true });
assert.equal(fs.readdirSync(output).length, 0, 'output directory must be empty');
const now = Date.parse('2026-03-01T12:00:00.500Z');
const realNow = Date.now;
Date.now = () => now;
function collector(name: string) {
  const home = join(output, name);
  fs.mkdirSync(home);
  const config = { enabled: true, filePath: join(home, 'stats.jsonl'), retentionMs: 60_000 };
  return { home, config, stats: new TokenStatsCollector(config) };
}
const input = Array.from({ length: 32 }, (_, index) => ({
  sessionId: `row-${index.toString().padStart(3, '0')}`,
  startedAt: '2026-03-01T11:59:30.000Z', endedAt: '2026-03-01T11:59:59.500Z',
  scenarioType: 'default', resolvedFrom: 'scenario', provider: 'openai', model: 'gpt-4o',
  usage: { inputTokens: index + 1, outputTokens: 2, nativeCost: (index + 1) / 1000 },
}));
let result: any;
try {
  const { home, stats } = collector('normal');
  const observed = structuredClone(input);
  for (const record of observed) stats.observe(record);
  const expected = {
    observed, snapshot: stats.snapshot(), hourly: stats.hourlySnapshots(),
    models: stats.modelUsageSnapshot(), recent: stats.recent(0),
    session: stats.sessionSnapshot(input[0].sessionId),
    journal: fs.readFileSync(join(home, 'stats.jsonl'), 'utf8'),
  };
  stats.clear();
  const cleared = stats.snapshot();
  stats.dispose();

  // Interpose only the filesystem rename seam: while real TS compaction holds
  // its real lock and has already read main, another real collector times out
  // and appends. No baseline source or dependency is modified.
  const race = collector('race');
  const writer = new TokenStatsCollector(race.config);
  const journal = join(race.home, 'stats.jsonl');
  const expired = { ...input[0], sessionId: 'expired', endedAt: '2000-01-01T00:00:00.000Z' };
  fs.writeFileSync(journal, JSON.stringify(expired) + '\n');
  const originalRename = fs.renameSync;
  let hits = 0;
  let beforeRename = '';
  fs.renameSync = ((from: any, to: any) => {
    if (to === journal) {
      hits++;
      writer.observe(structuredClone(input[0]));
      beforeRename = fs.readFileSync(journal, 'utf8');
    }
    return originalRename(from, to);
  }) as typeof fs.renameSync;
  syncBuiltinESMExports();
  let reader: any;
  try { reader = new TokenStatsCollector(race.config); }
  finally { fs.renameSync = originalRename; syncBuiltinESMExports(); }
  assert.equal(hits, 1, 'must reach the real TS compaction rename');
  assert.ok(beforeRename.includes(input[0].sessionId), 'TS fallback must append before replacement');
  const afterRename = fs.readFileSync(journal, 'utf8');
  assert.ok(!afterRename.includes(input[0].sessionId), 'must reproduce the TS loss window');
  result = { sourceCommit, sourceSha256, now, input, expected, cleared,
    tsRace: { beforeRename, afterRename, snapshot: reader.snapshot() } };
  reader.dispose(); writer.dispose(); race.stats.dispose();
} finally { Date.now = realNow; }
fs.writeFileSync(join(output, 'oracle.json'), JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
