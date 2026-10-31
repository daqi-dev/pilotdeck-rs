# SkillManager tree_files fixtures

Fixed baseline: `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, source
`src/extension/skills/SkillManager.ts:674-692` (buildSkillScanFolder's tree_files slice).
All expected values come from the real TS `SkillManager.scan` (`folders[0]`), no
implementation replication. Environment: Node v22 (asserted), the baseline lockfile's
tsx, `PDRS_BASELINE` per `../skills/README.md`, `LANG=en_US.UTF-8`.

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/skills-tree-files/generate.mts \
  /absolute/output/dir
```

The generator runs every case twice and requires both runs to serialize identically
(`JSON.stringify` byte comparison); it writes two files:

- `oracle.json` — portable cases. Fields: `sourceCommit`, `sourceSha256`,
  `cases[]: {name, fileCount, totalSize}` (all measured; entry counts are not
  observable from scan and are not recorded).
- `oracle-macos.json` — written only on darwin. Same fields plus
  `"platform": "darwin"`. These cases' values depend on the kernel symlink budget
  (macOS `MAXSYMLINKS=32`, Linux 40) and on the scratch prefix holding exactly one
  symlink (`/tmp -> /private/tmp`), so the generator builds them under literal
  `/tmp`. Per `../skills/README.md` such scenarios never enter the portable oracle.

Topology follows design v3 `probes/build.mjs` verbatim: link chains (`chain/`,
`hops/`, `real/`) live outside the skill root so only the link paths are
enumerated; DAG levels use absolute link targets, which re-resolve the scratch
prefix (2 links followed per level).

Rust replay tests (`cargo test -p pilotdeck-gateway --lib skills::import::tests`):

- `tree_files_matches_ts_baseline` — replays `oracle.json`, rebuilding each tree
  in a fresh temp dir and comparing `(fileCount, totalSize)` with
  `tree_files_capped(skill_root, TREE_FILES_CAP)`.
- `tree_files_matches_ts_baseline_macos` — same for `oracle-macos.json`, under
  `#[cfg(target_os = "macos")]`, with the tree rebuilt under literal `/tmp` (the
  one-symlink prefix the values assume).
- `tree_files_respects_cap_133` / `tree_files_exceeds_cap_132` — cap boundary at
  the parameter passed to `tree_files_capped` (no environment variable).
- `tree_files_wide_dir_stops_listing_at_cap` — 5001-entry flat directory at
  cap 100: asserts the `tree_files_limit_exceeded` error and, via a test-only
  thread-local `ReadDir::inspect` counter in `import.rs`, that the iterator
  yielded exactly cap+1 items; eager per-directory collection is observable.
- `tree_files_accumulates_cap_across_directories` — five subdirectories of 30
  files (156 entries total) at cap 100: the cap accumulates across directories,
  error at exactly entry 101.
- `scan_propagates_tree_files_limit_without_partial_folders` — exercises the scan
  implementation with an explicit small cap: a child overrun discards earlier
  folders; a root overrun also propagates through `scan_folder`. Public `scan`
  supplies `TREE_FILES_CAP` to this same implementation, without global overrides.
- `tree_files_deep_tree_with_low_fd_limit` — rebuilds deep-300, launches this test
  binary through a shell with `ulimit -n 16`, verifies the actual soft limit and
  compares with the real TS oracle. Parent enforces a 20-second deadline and
  kills/reaps the child; `tree_files_low_fd_child` is its subprocess entry point.
- `tree_files_fixture_regenerates_identically_from_ts` — `#[ignore]`, needs
  `PDRS_BASELINE`; regenerates both oracle files byte-identically.

| Case | oracle | fileCount / totalSize | Deepest lookup follows |
|---|---|---|---|
| dag-10 | portable | 1025 / 3130 | 21 links (macOS) / 11 (Linux) of 32/40 |
| linear-20 | portable | 21 / 78 | 21 links of 32/40 |
| hops-20 | portable | 2 / 61 | 22 links of 32/40 |
| deep-300 | portable | 2 / 63 | no links; ~660 B of the 1024 B macOS PATH_MAX |
| alias-10x5 | portable | 67 / 223 | 2 links |
| dag-15 | macos | 32769 / 98362 | 31 links of 32 — one below the macOS budget |
| dag-16 | macos | 1 / 58 | level-16 stat needs 33 links → ELOOP, only SKILL.md counts |
| dag-20 | macos | 1 / 58 | same truncation as dag-16 at feasible size |
| mutual | macos | 65 / 282 | expands until ELOOP cuts among real files |
| self-1 | macos | 64 / 2016 | expands until ELOOP cuts among real files |

Portable-case verification (measured, this macOS): scanning every portable case
through `/private/tmp` (no prefix symlink, budget freed by one) returns identical
values, so none touches the truncation boundary; the link arithmetic above leaves
≥ 10 links of margin on both platforms. The macOS cases are prefix-sensitive by
design: via `/private/tmp`, mutual returns 67/289, self-1 66/2079 and dag-16
65537/196666 — which is also what a root-canonicalizing implementation would
produce and what negative control NC-5 detects.

Negative controls (`python3 src/pilotdeck-gateway/tests/fixtures/skills-tree-files/negative.py`,
mutates a disposable copy of the tracked source (including pending repairs),
admits its Cargo target through the shared rule (unset defaults to the worktree
`_build/cargo-target`; a dedicated out-of-tree target is equally admissible), and runs
baseline → red → restored-green
for each control. SIGINT/SIGTERM unwind the owned child wait, kill/reap its session,
and remove the copy. The working source is never mutated. NC-5/NC-7 need macOS):

- NC-1 `no-cap` — remove the `listed.len() > cap` check → `tree_files_exceeds_cap_132` turns red.
- NC-2 `boundary` — change `>` to `>=` → `tree_files_respects_cap_133` turns red.
- NC-3 `collect-first` — eagerly `collect::<Vec<_>>()` each directory before the unchanged per-entry cap check → `tree_files_wide_dir_stops_listing_at_cap` turns red (counter 5001 ≠ 101).
- NC-4 `per-dir-count` — count per directory instead of accumulating → `tree_files_accumulates_cap_across_directories` turns red (no error).
- NC-5 `canonicalize-root` — canonicalize the root before walking → `tree_files_matches_ts_baseline_macos` turns red (dag-16 65537 ≠ 1).

- NC-6 `inode-dedup` — skip directories already seen by `(dev, ino)` → portable
  DAG oracle undercounts.
- NC-7 `inode-subtree-cache` — cache completed subtree counts by `(dev, ino)` →
  macOS mutual-cycle oracle disagrees as lookup contexts have different budgets.
- NC-8 `retain-parent-handles` — retain open `ReadDir`s while visiting the deep
  chain → low-fd test fails on fd exhaustion.
- NC-9 `swallow-scan-limit` — turn a `tree_files_capped` error into `(0, 0)` in
  `scan_folder` → scan propagation test fails.

Design-v3 acceptance reconciliation and original/new NC numbering are recorded
once in `specs/gateway-parity/diffs/A6.8-SKILLS-TREEFILES-002-R1.md`, under
“设计 v3 验收对账”. This suite does **not** claim every original planned test was
implemented; the explicit reductions and substitute evidence are listed there.
