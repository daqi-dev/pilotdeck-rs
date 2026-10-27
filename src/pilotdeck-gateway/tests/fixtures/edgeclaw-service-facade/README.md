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
| closed | `listReadableProjectCatalog` / `getReadableProject` / `listReadableProjectEntries` | — | succeeds on an empty catalog, `database is not open` once a project meta exists (the catalog reads `pipeline_state.workspaceDir`) | replayed |
| closed | `close` again (2nd, 3rd) | — | throws `Error: database is not open` | replayed |
| open → closed | `flush` (async) | close while the classify request is awaiting | promise rejects `Error: database is not open`; memory files already written stay | **N/A here**: async orchestration is A6.6-edgeclaw-19; the close-during-await cell is covered by #770's heartbeat matrix. Probe: `probe/close-during-await.mts` → `probe/during.json` |
| open | `flush` / `retrieve` / `dream` | — | not part of this package's facade | implemented by A6.6-edgeclaw-19 |

`updateProjectMeta` after close is the only closed-state mutator that succeeds:
it writes a memory file and does not touch SQLite.

## Files

- `observe.mts` — scenario table + driver. It imports the real service, runs
  deterministic seeds/operations and records return values, `pipeline_state`,
  `l0_sessions`, and every workspace/global file byte. `undefined` and
  non-finite numbers use a small `$type` envelope so JSON round-trips them.
- `generate.mts` — source-closure provenance (`bindSourceTree`) and `oracle.json`
  with `flag: 'wx'`; the output directory is created with `recursive: true`.
- `verify.mts` — re-runs the same scenarios and requires the Rust-produced
  `rust-observations.json` to equal both the oracle and the fresh TS run.
- `oracle.json` — committed goldens (10 scenarios).
- `negative.py` — six in-place mutations of `service/mod.rs`. It uses the
  shared `negative_process` guard (`mutating_sources` / `run_owned` /
  `wait_for_slot`), applies every mutant, records KILLED/SURVIVED/INVALID and
  exits non-zero on any non-KILLED. Each mutant was compile-checked before
  delivery; the official run needs the file to be committed first (the guard
  refuses an untracked product source).

## Commands

```sh
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_service_facade -- --include-ignored
```

The `#[ignore]` tests are the real-TS verifier and the
`regenerates_identically("edgeclaw-service-facade")` guard; both must be run
with `--include-ignored`.

Negative controls (after the file is in HEAD):

```sh
CARGO_TARGET_DIR=/abs/target python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/negative.py \
  _tmp/20261026-issue-775/negative-service-facade
```
