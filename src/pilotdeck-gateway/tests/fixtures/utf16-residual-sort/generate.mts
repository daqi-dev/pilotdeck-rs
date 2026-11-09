// #2131: real TS originals order the remaining JsString sort keys that carry
// isolated UTF-16 surrogates. Nothing is re-implemented here: every case
// imports the pinned baseline entry point and records its output.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#2131 Node pin');
assert.equal(process.versions.icu, '78.2', '#2131 ICU pin');
assert.ok(process.env.PDRS_BASELINE, '#2131 baseline required');
assert.ok(process.argv[2], '#2131 fresh output directory required');
const output = resolve(process.argv[2]);
mkdirSync(output, { recursive: true }); // oracle.json uses flag 'wx'
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/tool/registry/ToolRegistry.ts',
  'src/extension/plugins/runtime/PluginRuntime.ts',
  'src/context/cache/CachePlan.ts',
  'src/router/stats/TokenStatsCollector.ts',
]);

// Key sets come from the locale-invariant families of
// ../native-collation/oracle.json (61 keys x 14 locales, strict ascending in
// every locale): U+10000 < U+D800 < U+E000 < U+FFFD, and U+D800 < U+D801 <
// U+DC00. The generator re-verifies the invariance below, so the recorded
// orders hold under any process LANG on both sides. The case inputs are the
// same keys in a scrambled order, so a folded (U+FFFD) stable sort cannot
// reproduce the recorded order.
const QUARTET_CANONICAL = ['𐀀', '\ud800', '', '�'];
const LONE_UNITS_CANONICAL = ['\ud800', '\ud801', '\udc00'];
const QUARTET = ['', '\ud800', '�', '𐀀'];
const LONE_UNITS = ['\udc00', '\ud801', '\ud800'];
const LOCALES = [
  'en-US', 'zh-CN', 'de-DE', 'sv-SE', 'ja-JP', 'fr-CA', 'fr-Latn-CA',
  'fr-CA-u-co-emoji', 'fr-CA-u-co-eor', 'es-u-co-trad', 'lt', 'th',
  'en-u-kn-true', 'da',
];
for (const keys of [QUARTET_CANONICAL, LONE_UNITS_CANONICAL]) {
  for (const locale of LOCALES) {
    for (let i = 0; i < keys.length; i++) {
      for (let j = i + 1; j < keys.length; j++) {
        assert.ok(
          keys[i].localeCompare(keys[j], locale) < 0,
          `#2131 locale-invariant key family broken: ${locale} ${i}<${j}`,
        );
      }
    }
  }
}

// ToolRegistry.listAll / listUnavailableEntries (ToolRegistry.ts:118/179).
const { ToolRegistry } = await import(
  pathToFileURL(join(baseline, 'src/tool/registry/ToolRegistry.ts')).href
);
const tool = (name: string) => ({
  name,
  aliases: [] as string[],
  description: '',
  kind: 'builtin',
  inputSchema: {},
  isReadOnly: () => true,
  isConcurrencySafe: () => true,
  execute: async () => ({}),
});
const registryOrders = [QUARTET, LONE_UNITS].map(keys => {
  const registry = new ToolRegistry();
  for (const name of keys) registry.register(tool(name) as never);
  const listed = registry.listAll().map(entry => entry.name);
  for (const name of keys) {
    registry.markUnavailable(
      { toolName: name, code: 'unavailable', reason: 'r' },
      [],
    );
  }
  const unavailable = registry
    .listUnavailableEntries()
    .map(entry => entry.diagnostic.toolName);
  return { listed, unavailable };
});

// PluginRuntimeView.getAllMcpInstructions (PluginRuntime.ts:132-134 → :697).
const { PluginRuntimeView } = await import(
  pathToFileURL(join(baseline, 'src/extension/plugins/runtime/PluginRuntime.ts')).href
);
const instructionOrders = [QUARTET, LONE_UNITS].map(keys => {
  const mcpServers = Object.fromEntries(
    keys.map(name => [name, { instructions: 'i' }]),
  );
  const view = new PluginRuntimeView([
    { name: 'p', path: '/p', source: 'project', manifest: {}, mcpServers },
  ] as never);
  return view.getAllMcpInstructions().map(entry => entry.serverName);
});

// CachePlan.buildCachePlan (CachePlan.ts:49-67): the fingerprint hashes the
// sorted tools, so it is a byte-for-byte witness of the tool order.
const { buildCachePlan } = await import(
  pathToFileURL(join(baseline, 'src/context/cache/CachePlan.ts')).href
);
const cachePlans = [QUARTET, LONE_UNITS].map(keys =>
  buildCachePlan(
    {
      provider: 'p',
      model: 'm',
      systemPrompt: 'preview prompt',
      tools: keys.map(name => ({ name })) as never,
      messages: [],
      enabled: true,
    },
    1,
  )!.fingerprint,
);

// TokenStatsCollector.modelUsageSnapshot (TokenStatsCollector.ts:203-210).
const { TokenStatsCollector } = await import(
  pathToFileURL(join(baseline, 'src/router/stats/TokenStatsCollector.ts')).href
);
const scratch = mkdtempSync(join(tmpdir(), 'pdrs-utf16-residual-sort-'));
const usageOrders = [QUARTET, LONE_UNITS].map((keys, index) => {
  const collector = new TokenStatsCollector({
    enabled: true,
    // `filePath` only selects the directory (the journal is always
    // stats.jsonl inside it), so one directory per case: a shared directory
    // would replay case 1's records into case 2's snapshot.
    filePath: join(scratch, `case-${index}`, 'stats.jsonl'),
  });
  for (const name of keys) {
    collector.observe({
      sessionId: 's',
      scenarioType: 'custom' as never,
      resolvedFrom: 'direct' as never,
      provider: name,
      model: 'm',
      usage: {},
      startedAt: '2026-11-09T00:00:00.000Z',
      endedAt: '2026-11-09T00:00:01.000Z',
    } as never);
  }
  return collector
    .modelUsageSnapshot()
    .map(entry => entry.provider);
});

const cases = [QUARTET, LONE_UNITS].map((keys, index) => ({
  id: index === 0 ? 'lone-high-among-valid-code-points' : 'distinct-lone-code-units',
  input: keys,
  registry: registryOrders[index],
  mcpInstructions: instructionOrders[index],
  cacheFingerprint: cachePlans[index],
  modelUsage: usageOrders[index],
}));
const oracle = {
  node: process.version,
  icu: process.versions.icu,
  sourceCommit,
  sourceSha256,
  cases,
};
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle) + '\n', { flag: 'wx' });
process.stdout.write(
  JSON.stringify({
    node: process.version,
    icu: process.versions.icu,
    cases: cases.length,
    fingerprints: cachePlans,
  }) + '\n',
);
