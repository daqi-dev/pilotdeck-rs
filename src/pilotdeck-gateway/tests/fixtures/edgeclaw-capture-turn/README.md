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
- `negative.py` mutates `runtime/context/prepare.rs` and
  `host/modules/manifest.rs` in place (fork-carryover filter, always-on skip,
  error swallowing, `supports(CaptureTurn)`, manifest method) and requires each
  targeted Rust test to fail before restoring.

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
