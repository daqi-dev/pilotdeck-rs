# edgeclaw-service-facade

Real-TS goldens for `EdgeClawMemoryService` assembly and the synchronous facade
(A6.6-edgeclaw-18, baseline `cc7c4e6`). The generator imports the pinned
`src/context/memory/edgeclaw-memory-core/src/service.ts`, isolates HOME, freezes
`Date`/`Date.now`, fixes the `Math.random` sequence and passes fully explicit
LLM options. No HTTP request is made by any selected operation.

## Matrix (M18)

TS was measured on Node v22.22.0 before the Rust implementation. The raw probe
outputs live in `_tmp/20261026-issue-775/probe/`.

| state | event | timing | TS observation | Rust |
|---|---|---|---|---|
| open | constructor / paths / settings / captureTurn / reads / traces / export / import / clear / act | — | succeeds | replayed by `ts_service_facade_scenarios_match` |
| open | `close` | — | succeeds | replayed |
| closed | `getSettings`, `saveSettings`, `overview`, `snapshot`, `captureTurn`, `getSnapshotVersion`, all trace reads/writes, `export`, `import`, `clear`, `clearSession`, `act` | — | `Error: database is not open` | replayed |
| closed | `list`, `get`, `getUserSummary`, `getProjectMeta`, `getWorkspaceMode`, `updateProjectMeta` | — | still succeeds (file-store only; `getProjectMeta` is `undefined` when absent) | replayed |
| closed | `listReadableProjectCatalog` / `getReadableProject` / `listReadableProjectEntries` | — | succeeds on an empty catalog (`[]` / `undefined` / `[]`), `database is not open` once a project meta exists (the catalog reads `pipeline_state.workspaceDir`) | replayed by `closed-matrix` (meta exists) and `closed-empty` (no meta) |
| closed | `captureTurn` with no normalized messages (`[]` or tool-only) | — | returns `{captured: false, normalizedMessages: []}` without touching SQLite | replayed by `closed-empty`; a non-empty message still throws `database is not open` |
| closed | `close` again (2nd, 3rd) | — | throws `Error: database is not open` | replayed |
| open | `defaultIndexingSettings` non-finite (`NaN`, `+Infinity`) | — | `getSettings` preserves the JS number; `JSON.stringify` stores it as `null`; the null read falls back to the default, which keeps the non-finite value | replayed by `settings-nonfinite`; Rust storage uses `f64` |
| open | `defaultIndexingSettings` large finite (`1e20`) | — | `getSettings` preserves the JS number. Node v22.22.0: `JSON.stringify(1e20)` is `100000000000000000000` (only non-finite numbers become `null`); `saveSettings` then applies `clampInt`, so the persisted/read-back value is `10080` | replayed by `settings-large-finite`; Node probe `_tmp/20261026-issue-775/fix-r4/probe/stringify-1e20.out` |
| open | `saveSettings` interval `-0` / `"-0"` / `"-0.0"` | — | `clampInt` projects it to `+0` (`Object.is(value, -0) === false`; `JSON.stringify` emits `0`) | replayed by `save_settings_projects_negative_zero_like_ts`; Rust mirrors TS `Math.max(min, Math.min(max, …))`. Node v22.22.0 probe: `_tmp/20261026-issue-775/fix-r3/probe/negative-zero.json` |
| open | `get(ids, maxLines)` with `undefined` / `null` / `-0` | — | the TS default `maxLines = 80` applies only to `undefined`, so `undefined` returns the full body; `null <= 0` is true, so `null` returns records with `content: ""`. `-0` also takes the `<= 0` branch (empty) and is a finite zero that must encode as `{"$type":"number","value":"-0"}`, not `0` | replayed by `facade-sequence` (fix-r5 P2-1/P1); `undefined` keeps the body, `null` is empty, `-0` keeps the envelope on both sides |
| open | workspace data-dir slug | — | `sha1(resolve(workspaceDir)).slice(0, 10)` | both sides assert the slug before replacing it with `$HASH`; a wrong seed fails the comparison |
| open → closed | `flush` (async) | close while the classify request is awaiting | promise rejects `Error: database is not open`; memory files already written stay | **N/A here**: async orchestration is A6.6-edgeclaw-19; the close-during-await cell is covered by #770's heartbeat matrix. Probe: `probe/close-during-await.mts` → `probe/during.json` |
| open | `flush` / `retrieve` / `dream` | — | not part of this package's facade | implemented by A6.6-edgeclaw-19 |

`updateProjectMeta` after close is the only closed-state mutator that succeeds:
it writes a memory file and does not touch SQLite.

## Files

- `observe.mts` — scenario table + driver. It imports the real service, runs
  deterministic seeds/operations and records return values, `pipeline_state`,
  `l0_sessions`, and every workspace/global file byte. `undefined`, non-finite
  numbers and finite `-0` use a small `$type` envelope so JSON round-trips
  them.
- `generate.mts` — source-closure provenance (`bindSourceTree`) and `oracle.json`
  with `flag: 'wx'`; the output directory is created with `recursive: true`.
- `verify.mts` — re-runs the same scenarios and requires the Rust-produced
  `rust-observations.json` to equal both the oracle and the fresh TS run.
- `oracle.json` — committed goldens (13 scenarios).
- `negative.py` — twenty-two in-place mutations of `service/mod.rs`,
  `storage.rs`, `llm/config.rs` and the fixture encoder
  (plus the fix-r1 P0/P1/P2 regressions, fix-r3's `-0` projection,
  fix-r4's `interval`/`1e20`/retrieval-order checks and fix-r5's encoder /
  `get(null)` / `js_message` checks). Each mutant names the one test it must
  kill and the exact assertion message; the runner applies every mutant,
  records KILLED/SURVIVED/INVALID and exits non-zero on any non-KILLED. Each
  mutant was compile-checked before delivery; the official run needs the files
  to be committed first (the guard refuses an uncommitted product source).

## Commands

```sh
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_service_facade -- --include-ignored
```

The `#[ignore]` tests are the real-TS verifier and the
`regenerates_identically("edgeclaw-service-facade")` guard; both must be run
with `--include-ignored`.

Negative controls (after the file is in HEAD; `PDRS_BASELINE` is required by
the fixture-encoder mutant, which runs the ignored real-TS verifier):

```sh
PDRS_BASELINE=/abs/PilotDeck CARGO_TARGET_DIR=/abs/target python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/negative.py \
  _tmp/20261026-issue-775/negative-service-facade
```

Pass mutant names after the output directory to run a single binding (for
example `closed-capture-empty-not-early`). Without names the runner executes
the full set and still summarises every mutant before exiting.
