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
The registered divergences `A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001`
(#917, human decision #914 A) and `-002` live in `fix-917.json`: the same
six cells' inputs — the first five identical to their `oracle.json` twins,
the fix-r1 sixth cell carrying its own — but the cleared session's turn is
discarded before the persist (no file write, `storedResults=[]`,
`isNoOp=true`), while requests and `promptDebug` stay byte-identical to the
TS cells. Every other cell — including `clearSession` before `markL0Indexed`
and all `close` cells — still replays byte-identically against `oracle.json`.
The Rust recheck fails open on `StorageError::Closed` alone so the `close`
cells keep failing where TS fails; any other L0 read error fails closed with
the honest storage error.

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

fix-r3 narrows the clear discrimination from session level to per L0 row: the
mismatch arm and the in-lock recheck consult the removed-id set recorded at
clear time (`session_clear_removed_since`), so a turn captured after a
completed clear survives the recheck, and a heartbeat parked between its
baseline read and its list no longer lets a same-thread clear silently drop
that turn. The new `BeforeDeleteL0` seam (after `delete_memory_entries`,
before the L0 DELETE) and the `BeforeListUnindexed` heartbeat seam anchor the
two tests that pin this ordering (`clear_failing_before_l0_delete_keeps_
generation_and_turn` and `turn_captured_after_completed_clear_is_not_
silently_dropped`); the parked test now asserts the generation is still the
baseline while the clear is parked, so a pure fix-r2 rollback cannot pass it.

The negative controls mutate committed bytes, so `mutating_sources` refuses to
run on an uncommitted working tree. Eleven #917 mutants guard the fix-r1
through fix-r3 recheck: removing both layers (the round-1 shape, three edits),
hoisting the recheck in front of the classify await (three edits), letting the
in-lock recheck fire only while `storedResults` is empty, removing the in-lock
recheck alone, dropping the commit segment's snapshot scope, flipping the
`Closed` fail-open arm, treating a non-`Closed` L0 read error as not-cleared,
and the three clear-generation bumps — the failed-clear regression (early
SELECT + claim before the deletion), the parked-window pure rollback of the
fix-r2 bump move, and the bump hoisted in front of the L0 DELETE itself. The
first four must resurrect the cleared session's turn, the fifth must let a
concurrent clear run inside the commit segment, the sixth must drift a `close`
cell away from the TS baseline, the seventh must resurrect through a dropped
`l0_sessions` table, and the last three must lose the turn to a failed clear /
resurrect it across the parked clear's window / advance the generation before
the L0 deletion. Two further fix-r3 mutants make the recheck ignore the
removed-id set — the mismatch arm unconditionally (dropping the post-clear
captured turn) and the in-lock recheck comparing generations only — so both
layers must consult the recorded rows, not just the session generation.
Two #1207 mutants then guard the clear record itself: splitting the id
selection and the L0 DELETE back into two connection acquisitions (a capture
that starts while the clear is parked at the `L0IdsSelected` seam lands
between them and is deleted unrecorded) and reverting the per-row generation
map to the accumulating set (a row whose deleting clear predates the
baseline reads as cleared-since it), plus a third that drops the baseline
comparison outright — the re-captured turn is silently dropped behind the
stale entry. The #1207 fix-r1 review adds a fourth: dropping the re-arm from
the clear's abort path (`clear-abort-rearm-dropped`), so an indexed row whose
file the failed clear already removed is never re-listed and the file is
lost. The fix-r2 review adds a fifth (`clear-abort-delete-rearm-dropped`):
the same re-arm dropped from the entry-deletion error arm, the failure face
inside `delete_memory_entries` itself (a later unlink or the manifest repair,
after the files are already removed). Both blocking-witness tests
(`concurrent_capture_cannot_land_between_l0_id_selection_and_delete` and
`commit_segment_blocks_concurrent_clear_until_writes_complete`) judge through
doc-hidden lock-wait observers (`EdgeClawStorage::set_process_wait_observer`,
`FileMemoryStore::set_operation_wait_observer`), which fire only when a
caller really parks on the contended guard: the first probe event is either
the blocking witness or the other side's completion, so host load cannot
false-green their bound mutants and no wall-clock window participates in the
discrimination (the probe waits themselves only bound pathological
scheduling, like every other channel wait in the suite).
After commit:

```sh
CARGO_TARGET_DIR=/abs/cargo-target python3 tests/fixtures/edgeclaw-heartbeat-indexer/negative.py /abs/negative-out
```
