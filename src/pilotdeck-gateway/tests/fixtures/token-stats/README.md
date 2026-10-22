# token-stats fixtures

Real-TypeScript oracle for the Rust port of `TokenStatsCollector` and the
Gateway `usageSnapshotForSdk` / `modelUsageSnapshotForSdk` projections.

## Provenance

- Baseline tree: `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`
- Baseline commit: `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
- Hashed sources (recorded in `oracle.json.sourceSha256`):
  - `src/router/stats/TokenStatsCollector.ts`
    `886914cd342433be20c2ba58f832a3f59c91b88a69767fb459484d7b20475ad1`
  - `src/router/utils/modelPricing.ts`
    `f25e61b95a00e44765da0bb81b65b4fb988c886bbca97e03b9fd62b118c54c9c`
  - `src/cli/ProjectRuntimeRegistry.ts`
    `0df6b0e82be872c2a2d57f85e73136d75865ff1fc2c4ded2e5e5550d7a9353d`
- `oracle.json` sha256:
  `4cb2bdba2f6b095a1fc8188f859ec8489d4a980ed0f832363f80e08b98119b7d`

`generate.mts` imports the real modules and calls
`ProjectRuntimeRegistry.prototype.usageSnapshotForSdk` /
`modelUsageSnapshotForSdk` on a `{ resolve: () => ({ router: { stats } }) }`
receiver. Every expected record, aggregate, snapshot, projection and journal
byte comes from that run. Only the input event streams and checkpoints are
hand-authored.

## Regenerate

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
LANG=en_US.UTF-8 TMPDIR=/tmp \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/token-stats/generate.mts /tmp/run1
LANG=en_US.UTF-8 TMPDIR=/tmp \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/token-stats/generate.mts /tmp/run2
cmp /tmp/run1/oracle.json /tmp/run2/oracle.json
cp /tmp/run1/oracle.json src/pilotdeck-gateway/tests/fixtures/token-stats/oracle.json
```

The generator refuses an existing output directory (the oracle is written with
`wx`), so use a fresh directory for each run.

## Scenarios

| scenario | covers |
| --- | --- |
| `aggregate-and-projection` | multiple providers/models, scenario→fallback, subagent role, tiers, cache tokens, native `provider_reported`, `configured_price`, built-in/fallback/legacy sources, baseline model, failure-shaped usage, cross-day hourly buckets, session/project projections, reopen |
| `windows-and-pruning` | 505 events over 80 hourly buckets and 205 sessions: hourly cap 72, session cap 200 (evicted session `w115`, kept `w69`), recent-buffer trim 500→250, reopen rebuild sort |
| `retention-compaction` | seeded journal with expired (`2000`), retained (`2999`) and malformed `endedAt` rows; constructor compaction, lock cleanup, append, reopen |
| `json-migration` | one-time `stats.json` → `stats.jsonl`, sort by `startedAt`, `.bak` rename, then observe |
| `disabled` | no journal, empty aggregates, empty projections |
| `clear` | in-memory reset and journal truncation, surviving reopen |

Each step stores `expected.result` and, for explicit `files` checkpoints,
`expected.files` (every file in the scenario home). The replay also compares the
journal after every `observe` through the `files` checkpoints, so JSONL key order
and number formatting are pinned.

## Rust replay

`tests/token_stats.rs` replays the same steps through
`runtime::model::stats::TokenStatsCollector` and compares
`stringify_json` output byte-for-byte. Two additional tests drive
`RouterRuntime::execute` (success and terminal failure) with a scripted invoker
and assert that `observeRequest` lands in the collector and the JSONL.
