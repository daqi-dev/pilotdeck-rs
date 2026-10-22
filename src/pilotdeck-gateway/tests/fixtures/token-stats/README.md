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
    `0df6b0e82be872c2a2d57f85e73136d75865ff1fc2c4ded2e5e5550d7a9353d9`
  - `src/router/config/parseRouterConfig.ts`
    `cbee6bb146f2c2a933ea78011501b641f4bb7cef908ffb6ee365b820a9b37684`
  - `src/router/config/schema.ts`
    `69c8427d7d854eaed0d89475d6a97bfa8e9bd960deefd6e76de7e2f983056609`
- `oracle.json` sha256:
  `1fb59484c2544f51c0bc12edc4df87dbff97338b76691ad80e2e5beb6ab89f88`

`generate.mts` imports the real modules and calls
`ProjectRuntimeRegistry.prototype.usageSnapshotForSdk` /
`modelUsageSnapshotForSdk` on a `{ resolve: () => ({ router: { stats } }) }`
receiver. Every expected record, aggregate, snapshot, projection and journal
byte comes from that run. Only the input event streams and checkpoints are
hand-authored.

Retention interprets exactly the two canonical `toISOString()` shapes the
collector writes: 24-byte `YYYY-MM-DDTHH:mm:ss.sssZ` (years 0000-9999) and
27-byte `±YYYYYY-MM-DDTHH:mm:ss.sssZ` (outside that range, within the `Date`
limits). Other V8-parseable timestamps are kept. Rows whose V8 parse is a finite
past instant and which are *not* canonical are therefore not in this oracle
(TS would delete them) — they are pinned as "kept" in `stats_tests.rs` instead.
The divergence is disclosed in `specs/gateway-parity/accepted-diffs.md`
(A6.4-STATS-RETENTION-001) with the real-TS probe values; the decision behind it
is in `state/decisions-20261022.md`「A6.4-pr9b」.

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
| `retention-compaction` | seeded journal with expired (`2000`), retained (`2999`) and malformed `endedAt` rows: out-of-range offsets, `+24:00`, day `32`, `+汉a`, `00:00.500Z`, `24:00:00.0001Z`, `002000-...`, a real leap day (`2024-02-29`) and millisecond timestamps; constructor compaction, lock cleanup, append, reopen |
| `retention-boundary` | fixed `Date.now` (`now` in the oracle): `endedAt` exactly `now - retentionMs` is kept, one millisecond either side is kept/dropped; leap-day row dropped; reopen |
| `retention-expanded-years` | fixed `now` past the last valid `Date` instant, zero retention: canonical 27-byte `±YYYYYY` rows (`-000001`, `-010000`, `+010000`, min and max `Date`) expire; rows outside the `Date` range plus non-canonical expanded years (`-000000`, unsigned 6-digit) are kept; reopen |
| `session-request-log-cap` | 201 observations in one session: `requestLog` length 199/200/201 (200 kept, then `>200` → newest 100), reopen rebuild keeps 100 while aggregates still count 201 |
| `json-migration` | one-time `stats.json` → `stats.jsonl`, sort by `startedAt`, `.bak` rename, then observe |
| `disabled` | no journal, empty aggregates, empty projections |
| `clear` | in-memory reset and journal truncation, surviving reopen |

Each step stores `expected.result` and, for explicit `files` checkpoints,
`expected.files` (every file in the scenario home). The journal is compared at
those explicit `files` checkpoints (not after every `observe`), so JSONL key
order and number formatting are pinned there. `aggregate-and-projection` also
pins `estimateCost` for provider-reported (`nativeCost`, including `0`), pure
rate-card, and absent usage.

`oracle.json.configCases` records `parseRouterConfig` output for the stats
projection: string and object `baselineModel`, trim, rejected shapes,
`pricingNumber` filtering and the `retentionMs` safe-integer bound.

## Rust replay

`tests/token_stats.rs` replays the same steps through
`runtime::model::stats::TokenStatsCollector` and compares
`stringify_json` output byte-for-byte. Two additional tests drive
`RouterRuntime::execute` (success and terminal failure) with a scripted invoker
and assert that `observeRequest` lands in the collector and the JSONL.
