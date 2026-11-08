# always-on-2294 fixtures

Evidence for #2294 (A7-W7-pr3: always-on provider assembly + DiscoveryScheduler
periodic tick + stop/reload semantics), generated against the TS baseline
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` with Node v22.22.0.

- `generate.mts` — the baseline probe. Loads the real TS modules
  (`DiscoveryGates` / `DiscoveryScheduler` / `DiscoveryStateStore` /
  `WorkCycleStore` / `DiscoveryPlanStore` / `DiscoveryFire` lock pair /
  `AlwaysOnRuntime` / `AlwaysOnManager` / `ProjectAutomationBundle`) with the
  shared tsx loader and drives them with the narrow fakes each scenario needs.
  Same invocation contract as the other fixture generators: `node --import
  tsx/loader.mts generate.mts <out-dir>` with `PDRS_BASELINE` set (the
  `regenerates_identically("always-on-2294")` guard is the exact entry).
- `oracle.json` — the 58 scenario records the probe emits, byte-identical
  across three runs. The Rust replay tests assert against it: gate-chain
  reasons (`g*`), state-store normalization + exact `state.json` bytes
  (`s*`), discovery-lock wx semantics + payload bytes (`l1`), per-gate tick
  outcomes with no-lock/no-fire negatives (`t*`), the interval floor
  `max(1000, minutes * 60_000)` (`i*`), stop-waits-without-abort (`w1`),
  control-port envelopes (`r*`), dormancy signal handling (`d1`), and the
  provider start/stop/reload order matrix (`b*`).

Scenario groups map to the TS baseline as:

| group | TS source |
|---|---|
| `g*` | `src/always-on/runtime/DiscoveryGates.ts:26-99` |
| `s*` | `src/always-on/storage/DiscoveryStateStore.ts` |
| `l1` | `src/always-on/runtime/DiscoveryFire.ts:1145-1163` |
| `t*`, `i*`, `w1`, `d1` | `src/always-on/runtime/DiscoveryScheduler.ts` |
| `r*` | `src/always-on/runtime/AlwaysOnRuntime.ts:317-404`, `AlwaysOnManager.ts:185-215` |
| `b*` | `src/cli/ProjectAutomationBundle.ts` |

The probe writes its scratch trees under `<out-dir>/scratch/`; nothing here
depends on the absolute out-dir path except the `t3`/`d1` project keys, which
the Rust replay re-derives from the golden records themselves.
