# edgeclaw-service-facade

Real-TS goldens for `EdgeClawMemoryService` assembly and the synchronous facade
(A6.6-edgeclaw-18, baseline `cc7c4e6`). The generator imports the pinned
`src/context/memory/edgeclaw-memory-core/src/service.ts`, isolates HOME, freezes
`Date`/`Date.now`, fixes the `Math.random` sequence and passes fully explicit
LLM options. No HTTP request is made by any selected operation.

## Matrix (M18)

TS was measured on Node v22.22.0 before the Rust implementation. The raw probe
outputs live in `_tmp/20261026-issue-775/probe/`.

| state | event | timing | TS observation | Rust |
|---|---|---|---|---|
| open | constructor / paths / settings / captureTurn / reads / traces / export / import / clear / act | — | succeeds | replayed by `ts_service_facade_scenarios_match` |
| open | `close` | — | succeeds | replayed |
| closed | `getSettings`, `saveSettings`, `overview`, `snapshot`, `captureTurn`, `getSnapshotVersion`, all trace reads/writes, `export`, `import`, `clear`, `clearSession`, `act` | — | `Error: database is not open` | replayed |
| closed | `list`, `get`, `getUserSummary`, `getProjectMeta`, `getWorkspaceMode`, `updateProjectMeta` | — | still succeeds (file-store only; `getProjectMeta` is `undefined` when absent) | replayed |
| closed | `listReadableProjectCatalog` / `getReadableProject` / `listReadableProjectEntries` | — | succeeds on an empty catalog (`[]` / `undefined` / `[]`), `database is not open` once a project meta exists (the catalog reads `pipeline_state.workspaceDir`) | replayed by `closed-matrix` (meta exists) and `closed-empty` (no meta) |
| closed | `captureTurn` with no normalized messages (`[]` or tool-only) | — | returns `{captured: false, normalizedMessages: []}` without touching SQLite | replayed by `closed-empty`; a non-empty message still throws `database is not open` |
| closed | `close` again (2nd, 3rd) | — | throws `Error: database is not open` | replayed |
| open | `defaultIndexingSettings` non-finite (`NaN`, `+Infinity`) | — | `getSettings` preserves the JS number; `JSON.stringify` stores it as `null`; the null read falls back to the default, which keeps the non-finite value | replayed by `settings-nonfinite`; Rust storage uses `f64` |
| open | `defaultIndexingSettings` large finite (`1e20`) | — | `getSettings` preserves the JS number. Node v22.22.0: `JSON.stringify(1e20)` is `100000000000000000000` (only non-finite numbers become `null`); `saveSettings` then applies `clampInt`, so the persisted/read-back value is `10080` | replayed by `settings-large-finite`; Node probe `_tmp/20261026-issue-775/fix-r4/probe/stringify-1e20.out` |
| open | `saveSettings` interval `-0` / `"-0"` / `"-0.0"` | — | `clampInt` projects it to `+0` (`Object.is(value, -0) === false`; `JSON.stringify` emits `0`) | replayed by `save_settings_projects_negative_zero_like_ts`; Rust mirrors TS `Math.max(min, Math.min(max, …))`. Node v22.22.0 probe: `_tmp/20261026-issue-775/fix-r3/probe/negative-zero.json` |
| open | `get(ids, maxLines)` with `undefined` / `null` / `-0` / non-numbers | — | the TS default `maxLines = 80` applies only to `undefined`, so `undefined` returns the full body; `null <= 0` is true, so `null` returns records with `content: ""`. `-0` also takes the `<= 0` branch (empty) and is a finite zero that must encode as `{"$type":"number","value":"-0"}`, not `0`. Every other value is coerced by `trimContentLines`' relational comparisons: `false`/`""`/`-Infinity` → empty, `true`/`"1"` → one line plus `...`, `NaN`/`"abc"` → `"\n..."`, `Infinity` → full body, `1.5`/`"1.5"` → one line plus `...` | replayed by `facade-sequence` (fix-r5 P2-1/P1, fix-r6 P1); Node v22.22.0 probe `_tmp/20261026-issue-775/fix-r6/probe/ts-maxlines.out` |
| open | workspace data-dir slug | — | `sha1(resolve(workspaceDir)).slice(0, 10)` | both sides assert the slug before replacing it with `$HASH`; a wrong seed fails the comparison |
| open → closed | `flush` (async) | close while the classify request is awaiting | promise rejects `Error: database is not open`; memory files already written stay | **N/A here**: async orchestration is A6.6-edgeclaw-19; the close-during-await cell is covered by #770's heartbeat matrix. Probe: `probe/close-during-await.mts` → `probe/during.json` |
| open | `flush` / `retrieve` / `dream` | — | not part of this package's facade | implemented by A6.6-edgeclaw-19 |

`updateProjectMeta` after close is the only closed-state mutator that succeeds:
it writes a memory file and does not touch SQLite.

## Files

- `observe.mts` — scenario table + driver. It imports the real service, runs
  deterministic seeds/operations and records return values, `pipeline_state`,
  `l0_sessions`, and every workspace/global file byte. `undefined`, non-finite
  numbers and finite `-0` use a small `$type` envelope so JSON round-trips
  them.
- `generate.mts` — source-closure provenance (`bindSourceTree`) and `oracle.json`
  with `flag: 'wx'`; the output directory is created with `recursive: true`.
- `verify.mts` — re-runs the same scenarios and requires the Rust-produced
  `rust-observations.json` to equal both the oracle and the fresh TS run.
- `oracle.json` — committed goldens (13 scenarios).
- `negative.py` — twenty-four in-place mutations of `service/mod.rs`,
  `storage.rs`, `file_memory.rs`, `llm/config.rs` and the fixture encoder
  (plus the fix-r1 P0/P1/P2 regressions, fix-r3's `-0` projection,
  fix-r4's `interval`/`1e20`/retrieval-order checks, fix-r5's encoder /
  `get(null)` / `js_message` checks and fix-r6's non-number `maxLines`
  checks). Each mutant names the one test it must kill and the exact assertion
  message; the runner applies every mutant, records KILLED/SURVIVED/INVALID and
  exits non-zero on any non-KILLED. A classifier self-test first proves a
  temp-file / source-guard Node failure stays INVALID while the dedicated
  `#775 negative-zero encoding mismatch at <path>` line is KILLED. Each mutant
  was compile-checked before delivery; the official run needs the files to be
  committed first (the guard refuses an uncommitted product source).

## Commands

```sh
PDRS_BASELINE=/abs/PilotDeck cargo test -p pilotdeck-gateway \
  --test edgeclaw_service_facade -- --include-ignored
```

The `#[ignore]` tests are the real-TS verifier and the
`regenerates_identically("edgeclaw-service-facade")` guard; both must be run
with `--include-ignored`.

Negative controls (after the file is in HEAD; `PDRS_BASELINE` is required by
the fixture-encoder mutant, which runs the ignored real-TS verifier):

```sh
PDRS_BASELINE=/abs/PilotDeck CARGO_TARGET_DIR=/abs/target python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/negative.py \
  _tmp/20261026-issue-775/negative-service-facade
```

Pass mutant names after the output directory to run a single binding (for
example `closed-capture-empty-not-early`). Without names the runner executes
the full set and still summarises every mutant before exiting.

## #988 预编译试点

`pdrs-mutants` 不在默认 feature 图中。`mutants::on` 只有在库内
`cfg(all(test, feature = "pdrs-mutants"))` 下读取 `PDRS_MUTANT`；普通库、
集成测试链接的库及 release 产品库始终使用 `const false`，即使显式打开
feature 也不会激活。既有测试文件在 feature 下同时作为库内的
`mutant_facade` 模块编译，避免 integration 依赖库没有 `cfg(test)` 的陷阱。
其中两条切换进程 cwd 的测试在 feature 下标为 ignored，避免与库内其它测试
交错；新执行器使用 `--exact --include-ignored` 在独立进程中逐条执行它们。

`schemata.py` 从 Cargo JSON 获取本次 feature 库测试的 executable，只编译
一次；24 项复用 `negative.py` 的名称、绑定测试、专属断言、witness 和共享
`negative_suite.classify`。保留 fixture panic 文件约束（库内 inclusion 的
路径显式为 `src/../tests`）。每项启动独立进程，正控和恢复正控清除选择变量；
超时/错断言/编译错误记 INVALID 后继续，最后任一非 KILLED 即非零退出。
旧执行器也在正控、witness、编译、变异、恢复各阶段捕获超时并继续；若源码
锁或恢复失败，则停止进一步改源码，把所有结果记 INVALID 并完整汇总。
TS encoder 变异只由 gated Rust verifier 显式向 Node 传独立标记。

```sh
# 在仓库根目录；调用方设置独立 CARGO_TARGET_DIR 和两个 line-tables-only profile。
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/schemata.py --check
PATH=/path/to/node-v22.22.0/bin:$PATH PDRS_BASELINE=/abs/PilotDeck python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/schemata.py NEW_OUTPUT_DIR
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/test_schemata.py
```

新执行器不改源码，用共享 tree lock 排斥旧执行器，并记录 HEAD、源码哈希、
二进制路径/哈希、逐次命令/退出码/计时、原始输出与原子发布的 summary。
它允许验证未提交的实现，但记录明确不宣称这是同 HEAD 等价证据。
旧 `negative.py` 继续受原始 `mutating_sources(..., baseline='HEAD')` 保护；
锚点同步到未启用分支，仍通过原地替换制造既有故障，不靠改选择开关替代旧机制。
提交后在同一 HEAD 分别运行两者，对比 summary 的 `(name, status)` 集合及总耗时。
完整旧机制预计超过 20 分钟时在协调者宿主执行，不在 worker 后台续跑。

`switch_negative.py` 为默认恒 false / 精确 selector 两条新增检查提供源码
负控；必须提交后才能运行（新 mutants.rs 不在 HEAD 时拒跑）。每个变异
先 no-run 再运行绑定测试，异常汇总并恢复。新机制一次 no-run 已同时类型检查
全部 23 个 Rust 分支；第 24 项 TS encoder 由 ignored verifier 实跑。
