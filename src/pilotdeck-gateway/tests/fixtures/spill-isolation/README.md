# Shared spill-root isolation (#638/#690)

`oracle.json` records the fixed cc7c4e6 TypeScript under Node v22.22.0. The
24 rows cross root/ID relation, scheduling, and budget ownership. Four rows
cannot be constructed because a single budget has one immutable root.
The injected spill port pauses one actual file write after four of eight
bytes; all other calls use the baseline native port. Cancellation means
abandoning observation of the TS promise. Gates are released only after the
observed follower outcome, and every probe has a 10-second watchdog.

Generate into a fresh directory (the generator refuses overwrite):

```sh
PDRS_BASELINE=/path/to/PilotDeck node --import /path/to/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/spill-isolation/generate.mts /fresh/output
```

`tests/tool_budget.rs::spill_isolation_regenerates_identically` is an ignored
regeneration guard. `runtime/context/spill_isolation_tests.rs` has one test per
matrix cell, plus public media-path recovery and context-isolation controls.
The Rust behavior intentionally differs in the defective TS cells; registration:
`specs/gateway-parity/diffs/A7.4-TOOLBUDGET-CONTEXT-ISOLATION-001.md` (unapproved).

From a committed candidate, run `python3 .../negative_controls.py /fresh/logs`.
The Cargo target goes through the shared admission (unset defaults to this worktree's
`_build/cargo-target`; a dedicated out-of-tree target is equally admissible).
It uses the shared `mutating_sources`, `wait_for_slot`, and `run_owned` owners,
requires assertion failures (not compile failures), and restores checked bytes.
An uncommitted sandbox candidate can supply `PDRS_NEGATIVE_BASELINE` with its
candidate tree object and isolated `GIT_INDEX_FILE`/object directory; the same
byte checks remain mandatory. See the task RESULT for the exact tested command
and isolated-index procedure. The normal repository index is never changed.
