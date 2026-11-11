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

| group | cells | TS source |
|---|---|---|
| `g*` | 18 | `src/always-on/runtime/DiscoveryGates.ts:26-99` |
| `s*` | 10 | `src/always-on/storage/DiscoveryStateStore.ts` |
| `l1` | 1 | `src/always-on/runtime/DiscoveryFire.ts:1145-1163` |
| `t*`, `i*`, `w1`, `d1` | 16 | `src/always-on/runtime/DiscoveryScheduler.ts` |
| `r*` | 2 | `src/always-on/runtime/AlwaysOnRuntime.ts:317-404`, `AlwaysOnManager.ts:185-215` |
| `b*` | 11 | `src/cli/ProjectAutomationBundle.ts` |

The probe writes its scratch trees under `<out-dir>/scratch/`; nothing here
depends on the absolute out-dir path except the `t3`/`d1` project keys, which
the Rust replay re-derives from the golden records themselves.

## Negative controls

`negative_controls.py` mutates one anchored block of the ported sources at a
time, re-runs the single bound named test, and requires the mutant to be
killed by that test's own `#2294` panic message. Every bound test replays
committed oracle bytes or asserts through the fixture harness, so every row
runs on both Linux and macOS:

```
python3 -I src/pilotdeck-gateway/tests/fixtures/always-on-2294/negative_controls.py --check
CARGO_TARGET_DIR=/absolute/build/dir python3 -I src/pilotdeck-gateway/tests/fixtures/always-on-2294/negative_controls.py /tmp/fresh-always-on-negative [mutation ...]
```

Run with no other writers in this worktree: the runner rewrites the guarded
sources in place, and `CARGO_TARGET_DIR` only isolates build artifacts, not
source files — a plain `cargo test` / `cargo build` sharing this tree during
the mutation window compiles a mutant and produces unrelated false failures.
The exclusive `.negative-source.lock` orders negative-control
`mutating_sources` writers among themselves; Rust read-lease consumers
(`tests/common/read_lease.rs` in the gateway and cli crates) take a shared
lock on the same file, so a writer also excludes those readers. It does not
block ordinary cargo runs or lock-free Python checkers, and its release
(e.g. after a SIGKILLed runner) does not prove the tree is clean.

`--check` verifies every anchor matches the tree exactly once, and that the
hand-copied numbers still describe the artifacts they name: the README
scenario total and group-table counts equal the `oracle.json` records, the
Rows list below equals this file's `mutations` in order, every oracle
scenario has a bound replay in the always-on sources, and every row's bound
test is actually defined there. The full run writes per-mutant logs plus
`summary.json` into `OUT` and exits nonzero unless every row is KILLED.
Rows: `gate-chain-lock-gate-removed`, `stop-order-swapped`,
`reload-rebuilds-unchanged-family`, `advertise-without-manager`,
`fired-tick-keeps-the-lock`, `watch-start-before-slot-store`.
