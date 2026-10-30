# edgeclaw-capture-turn

Real-TS goldens for `DefaultContextRuntime.captureTurn`,
`canonicalMessagesToMemoryMessages` and the advertised context method table
(#759, baseline `cc7c4e6`).

- `generate.mts` imports the pinned `src/context/DefaultContextRuntime.ts` and
  `src/context/memory/MemoryResolver.ts`, runs a recording / throwing /
  rejecting `MemoryResolver`, and builds the manifest with the real
  `createSidecarDefaultModuleDispatcher`. `sourceSha256` is the relative-import
  closure of both entries.
- `oracle.json` stores the observations (`scenarios`, `conversions`,
  `manifest`) plus the exact input specs that produced them.
- `observe.mts` holds the shared scenario/spec table and runners.
- `verify.mts` re-runs the same TS scenarios and requires the Rust-produced
  `rust-observations.json` to match both the oracle and the fresh TS run.
- `negative.py` mutates `runtime/context/prepare.rs`,
  `host/modules/manifest.rs` and this fixture's `oracle.json` in place
  (fork-carryover filter, always-on skip, sessionId rejection and its TS error
  text, `errored` forwarding, error swallowing, `supports(CaptureTurn)`,
  manifest method, oracle scenario/call truncation) and requires each targeted
  Rust test to fail before restoring. The runner takes only the output
  directory: per-mutant sample selections were removed after their verdict
  scaffolding kept failing review, and the full table is the only accepted
  evidence (#786 fix-r2).
- `recorded_call_key_sets_match_ts` compares each recorded resolver call's own
  key set against the TS recorder's, which JSON comparison cannot see; the
  oracle-truncation mutants above guard that comparison against a silently
  shorter or empty zip side (#786).

Run the generator with Node 22 and the baseline's `tsx`:

```sh
env -u LC_ALL PDRS_BASELINE=/abs/PilotDeck LANG=en_US.UTF-8 \
  node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  generate.mts /abs/output
```

Rust replay:

```sh
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_capture_turn -- --include-ignored
```
