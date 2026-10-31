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

`oracle.json` is untouched by #917: the five "resurrected" cells above stay
byte-identical TS observations, and the `#[ignore]` regeneration guard,
`verify.mts` and the #280 regeneration-negative controls all keep passing.
The registered divergence `A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001`
(#917, human decision #914 A) lives in `fix-917.json`: the same five cells'
inputs, but the cleared session's turn is discarded before the persist (no
file write, `storedResults=[]`, `isNoOp=true`), while requests and
`promptDebug` stay byte-identical to the TS cells. Every other cell —
including `clearSession` before `markL0Indexed` and all `close` cells — still
replays byte-identically against `oracle.json`. The Rust recheck fails open on
`StorageError::Closed` alone so the `close` cells keep failing where TS fails;
any other L0 read error fails closed with the honest storage error.

fix-r1 adds the sixth cell `matrix-general-select-clearSession`: the TS
baseline never recorded it in `oracle.json`, so the cell carries its own input
(the `matrix-general-select-clearProject` input with the action changed) and
its TS evidence under `ts` — the gated in-flight probe observation from
`_tmp/20261026-issue-917/fix-r1/probe-select-clearsession.mts` (Node v22.22.0,
baseline `cc7c4e6`), which still resurrects `Project/alpha-38e115ba98.md`
while the fixed Rust replay writes nothing. Input fields, request sequence
and the classify/create `promptDebug` are field-for-field identical between
the two sides; the divergence drops the routing step's `promptDebug` together
with the discarded persist, and the seeded `GeneralProjects/` meta survives on
both sides (registered in the `-002` diff ledger entry).

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
run on an uncommitted working tree. Seven #917 mutants guard the fix-r1
recheck: removing both layers (the round-1 shape, three edits), hoisting the
recheck in front of the classify await (three edits), letting the in-lock
recheck fire only while `storedResults` is empty, removing the in-lock recheck
alone, dropping the commit segment's snapshot scope, flipping the `Closed`
fail-open arm, and treating a non-`Closed` L0 read error as not-cleared. The
first four must resurrect the cleared session's turn, the fifth must let a
concurrent clear run inside the commit segment, the sixth must drift a `close`
cell away from the TS baseline, and the last must resurrect through a dropped
`l0_sessions` table. After commit:

```sh
CARGO_TARGET_DIR=/abs/cargo-target python3 tests/fixtures/edgeclaw-heartbeat-indexer/negative.py /abs/negative-out
```
