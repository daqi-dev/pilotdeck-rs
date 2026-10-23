# SkillManager tree_files fixtures

Fixed baseline: `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`, source `src/extension/skills/SkillManager.ts:674-692` (buildSkillScanFolder's tree_files slice). All outputs from real TS `fs.readdir({ recursive: true })` + stat loop, no implementation replication. Environment: Node v22.22.0, baseline lockfile's tsx, `PDRS_BASELINE` per `../skills/README.md`.

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL= node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/skills-tree-files/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/skills-tree-files
```

`oracle.json.cases[]`: `name`, `setup` (symlink tree), `ts_output` (entries, fileCount, totalSize from real TS), `source_commit`, `source_sha256`. Generator runs each case twice and asserts byte-identical JSONL (excluding elapsed). Provenance guard binds baseline commit and source hash.

Rust test `skills::tree_files_matches_ts_baseline` replays all cases: builds symlink tree in scratch, calls `tree_files`, compares (fileCount, totalSize) to TS oracle. Cap tests use `TREE_FILES_CAP_OVERRIDE`.

| Case | Coverage |
|---|---|
| dag-10 | Exponential DAG k=10, 1024 files, 3071 entries |
| dag-15 | Exponential DAG k=15, 32768 files, 98303 entries |
| dag-20 | ELOOP截断：131071 entries but 0 files (all stat ELOOP) |
| deep-300 | 300层无链接目录，302 entries, 1 file |
| linear-20 | 20-hop linear symlink chain, 41 entries, 21 files |
| hops-20 | 20-hop chain to single file, 3 entries, 2 files |
| alias-10x5 | 10 symlinks × 5 subdirs (multi-alias), 133 entries, 67 files |
| mutual | 2 mutual symlinks, 131 entries, 65 files (ELOOP at depth 32 macOS) |

Platform-specific: self-referencing symlinks expand to MAXSYMLINKS (macOS 32, Linux 40). ELOOP depth path-dependent. TS behavior on these inputs matches current platform, not portable across OS.

Negative controls (`negative.py`):
- NC-1: Remove cap check → self-ref timeout
- NC-2: Change `>` to `>=` → boundary case fails
