# edgeclaw-export

Golden fixture for `MemoryRepository` export/import, overview, UI snapshot and
list/count (`core/storage/sqlite.ts:1495-1775` and its helpers), built like
`../edgeclaw-accessors`:

- `generate.mts` runs fixed real TS (commit cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85,
  provenance-guarded) with a frozen clock (`2026-10-22T00:00:00.000Z`) and
  `HOME` under a temp root, seeds workspaces/global memory (including the
  managed user profile), `workspaceDir` pipeline state (so the general repo's
  catalog lists the sibling workspace stores as `workspace_external` entries
  that `getOverview` must exclude), pipeline state, traces, L0 sessions and
  `last_dream` snapshots, then records one observation per operation plus the
  final file tree into `oracle.json`.
- The seeding phase runs on the frozen clock; the observed operations then
  switch to a stepping clock (`stepClock` in `../edgeclaw-file-memory/observe.mts`:
  every `Date` read advances +1 ms from the frozen base). Timestamps in the
  observations therefore pin the *number and order* of `nowIso()`/`Date.now()`
  reads — e.g. `exportedAt` is exactly 2 reads after the previous op, and a
  full import of the saved bundle consumes 14 reads before `importedAt`.
  `verify.mts` and the Rust replay must run the same two-phase clock.
- JSON cannot carry `NaN`: op inputs use the string marker `"$NAN"`, expanded
  to a real `NaN` number by `expand()` in `observe.mts` and by `expand_js` in
  the Rust replay. The NaN pagination inputs (UI snapshot `limit`, list
  `limit`/`offset`) pin the JS `Math.max`-keeps-NaN behavior.
- The L0 seed includes an indexed-only session key and two unindexed rows
  sharing one key, so `pendingSessions` detects both a dropped
  `WHERE indexed = 0` and a dropped `COUNT(DISTINCT …)` (both must stay 3 for
  the main repo).
- Mixed-error import inputs (duplicate path + invalid record, legacy path +
  missing content) pin the `files.map(...)`-before-duplicate-check error
  precedence.
- The Rust replay (`tests/edgeclaw_export.rs`) restores the TS databases and
  file tree (or re-seeds everything through the Rust implementation) and
  compares every observation and the final tree.
- `verify.mts` replays the oracle's operations against real TS on the
  Rust-seeded root, proving the Rust-written databases and files round-trip.
- `projectId` list/count combinations run un-gated since PR #525 / #517
  removed the Rust `listGlobalMemoryEntries` projectId passthrough: the
  repository methods forward the id only to the workspace store
  (sqlite.ts:1733-1737 / 1762-1766), and the global path's whitelist
  (sqlite.ts:894-901) drops it — on the project repos a foreign id empties
  the workspace side, on the general repo it selects one project's entries.
- Review round 5 pins the type-coercion edges of import validation and
  pagination: non-string bundle scopes (`42`, `true`, `{}`, `0`, `false`,
  `""`, `null`) import like a missing one (the scope check reads strings
  only, sqlite.ts:301-305) and the scope error outranks a bad
  `formatVersion`; non-string `relativePath`/`content` stay invalid. List /
  UI-snapshot inputs with non-number `limit`/`offset` (`'3'`, `'abc'`,
  `true`, `null`, `[5]`, `[1,2]`) pin the JS ToNumber coercion of
  `Math.max(0/1, … ?? …)` — only `null`/`undefined` fall back to the
  defaults, and the UI snapshot's `limit = 50` default applies only to a
  missing argument, so `null` lists everything.
- Review round 6 pins ToNumber's ToPrimitive exceptions and the untrimmed
  scope check: `limit`/`offset` wire objects with an own `toString`
  property (`{toString:null}`, `{toString:null,valueOf:null}` — JSON cannot
  hold a callable, so it shadows the prototype method with a non-callable)
  throw `TypeError: Cannot convert object to primitive value`, directly and
  inside arrays, while `{valueOf:null}` and `{}` convert to NaN like any
  other object. Callables (`{valueOf:()=>3}` → 3, `{toString:()=>"4"}` → 4,
  probed against real TS) cannot cross the JSON wire, so they are unreachable
  through the gateway and not in the oracle. Bundle scopes `' current_project '`,
  `'  '` and `'current_project '` stay rejected: `normalizeString` does not
  trim (sqlite.ts:141-143, unlike the timestamp `.trim()` call sites).

Regenerate: `node --import <baseline>/node_modules/tsx/dist/loader.mjs generate.mts <out-dir>`
with `PDRS_BASELINE` set (see `tests/common/baseline.rs`; the ignored
`regenerates_identically` test does exactly this and diffs the bytes).


Round 9 lock regression: `generate_locks.mts` uses the same fixed-source guard
and real public TS constructors plus `setPipelineState("workspaceDir", ...)`.
It records `lock-oracle.json` for general/single, two general repositories,
and a three-repository cycle whose middle repository's global root is the
third repository's memory root. Both cold and warmed TS calls must return the
same observations. Regenerate with the command above, substituting
`generate_locks.mts`; sha256 is
`21dfe8e71916697a67a3d248c3803c1e57eed5dbeb67688219d6d5da961f4e51`.

`storage/overview_lock_tests.rs` coordinates actual root lock acquisitions
using cfg(test) thread-local hooks, covers both standalone overview and UI
snapshot with cold/warm caches, and compares returns to these real TS values.
The parent kills/reaps a child after 20 s only to prevent a deadlocked mutant
from hanging the suite. `negative_locks.py <evidence-dir>` temporarily restores
r6/r7 scan order, removes root sorting, holds E over construction, and restores
an outer lock on standalone overview; all source bytes are restored in finally.
The r7 general/single case completes but violates the no-outer-lock invariant;
its negative result is an invariant assertion, not a claimed deadlock. Other
r6/r7 cyclic layouts fail by the parent deadline. Builds inherit the launcher's
target directory (otherwise Cargo's existing `src/target`); no second target is
created. Optional trailing `r6`, `r7`, `overview`, or `snapshot` selects a repair mutant.
