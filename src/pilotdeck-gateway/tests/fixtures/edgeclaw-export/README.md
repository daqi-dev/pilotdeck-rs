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
- The Rust replay (`tests/edgeclaw_export.rs`) restores the TS databases and
  file tree (or re-seeds everything through the Rust implementation) and
  compares every observation and the final tree.
- `verify.mts` replays the oracle's operations against real TS on the
  Rust-seeded root, proving the Rust-written databases and files round-trip.

Regenerate: `node --import <baseline>/node_modules/tsx/dist/loader.mjs generate.mts <out-dir>`
with `PDRS_BASELINE` set (see `tests/common/baseline.rs`; the ignored
`regenerates_identically` test does exactly this and diffs the bytes).
