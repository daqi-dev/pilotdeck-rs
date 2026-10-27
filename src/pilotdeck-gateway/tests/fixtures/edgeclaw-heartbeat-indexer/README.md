# edgeclaw-heartbeat-indexer

Real-TS goldens for the stateful `HeartbeatIndexer` (#770, baseline
`cc7c4e6`). `generate.mts` drives the unmodified
`src/context/memory/edgeclaw-memory-core/src/service.ts` with:

- `captureTurn` for the acceptance scenarios and direct `captureL0Session`
  for duplicate suppression;
- a loopback scripted LLM (`../edgeclaw-llm/stub.mts`) replaying the same
  classify/create/select sequence on both sides;
- a frozen clock, a deterministic `Math.random`, and an isolated `HOME` set
  before importing `general-projects.ts`;
- deterministic hooks at the M13 cells: `afterList`, awaited `classify`,
  awaited `createNote`, awaited `selectIndexProject`, and before
  `markL0Indexed`, with `captureTurn`, `clearSession`, `clear(current_project)`
  and `close` events.

`oracle.json` records every L0 row, stats/rejection, pipeline state, index
trace (including every `promptDebug` system/user prompt and raw response),
memory/global file bytes, and every request method, URL, header and raw JSON
body. `tests/edgeclaw_heartbeat_indexer.rs` replays the same
cases through `HeartbeatIndexer`; its `#[ignore]` regeneration guard reruns
`generate.mts` and requires byte-identical output.

The M13 matrix observed from this real TS baseline:

| cell | observation |
|---|---|
| capture during awaited classify | new L0 stays `indexed=0`; next run processes it |
| clearSession during awaited classify/create | the turn's file is written after the clear ("resurrected"); L0 rows stay deleted |
| clearSession before `markL0Indexed` | the already-written file is removed; trace still records it |
| clear(current_project) before `markL0Indexed` | files and pipeline state removed |
| close during awaited classify/create/select | per-session catch warns; final trace save rejects with `database is not open` |
| close before `markL0Indexed` | `markL0Indexed` rejects immediately, no per-session warning |

Run the generator with Node 22 and the baseline's `tsx`:

```sh
env -u LC_ALL PDRS_BASELINE=/abs/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  generate.mts /abs/output
```

Rust replay:

```sh
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_heartbeat_indexer -- --include-ignored
```

Two cases carry a candidate name cut at 80 UTF-16 units
(`'a'.repeat(79) + '😀'`), so the trace keeps the lone high surrogate exactly
as Node does; a third carries an untruncated non-BMP name.

The negative controls mutate committed bytes, so `mutating_sources` refuses to
run on an uncommitted working tree. After commit:

```sh
CARGO_TARGET_DIR=/abs/cargo-target python3 tests/fixtures/edgeclaw-heartbeat-indexer/negative.py /abs/negative-out
```
