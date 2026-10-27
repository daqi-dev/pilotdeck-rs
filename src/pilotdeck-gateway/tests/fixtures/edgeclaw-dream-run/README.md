# EdgeClaw DreamRewriteRunner.run (A6.6-edgeclaw-17, #774)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
的 `core/review/dream-review.ts:1026-1492`：`DreamRewriteRunner.run()` 总编排
（快照、General 元合并、各类别、元审查、用户画像重写、manifest 修复、
no-op、summary、`DreamTraceRecord`）。同文件 `543-798` 的
`runCategoryDream` 错误退出路径是本包同时清理的 #915 金样格。

`observe.mts` 直接 import 该提交的 `DreamRewriteRunner` 与 `MemoryRepository`，
用真实 SQLite + `FileMemoryStore` 造 single / general 两种 stage 仓，再用脚本化
extractor 回放五个 LLM 方法（含 `rewriteUserProfile`）。`HOME` 隔离到
`$SCRATCH/home`，`GENERAL_WORKSPACE_DIR` 在首次 import 前固定；时钟冻结在
`2026-10-22T00:00:00.000Z`；`Math.random` 第一条消费给 `createDreamTrace` 的
radix-36 后缀，之后每个 mutation 消费一条。

每个用例记录：

- `expected.result`：`run()` 返回值（含完整 trace、`isNoOp`、`summary`）；
- `expected.trace`：同一 trace（单独再比一次，避免 result 结构漂移掩盖 trace）；
- `expected.calls`：extractor 收到的每个 LLM 输入（绝对路径清成 `$ROOT`）；
- `expected.logs`：`logger.warn` 原文；
- `expected.tree.workspace` / `expected.tree.global`：两个 store 的文件树；
- `category-warning-then-error` 还记录 `expected.error`（`String(error)`）和
  出错时的 trace：TS 的 plan-output detail 持有 live `droppedWarnings` 数组，
  所以后一个 cluster 抛错时，前一个 cluster 的 “fewer than 2 files could be
  loaded” 警告已经可见。Rust 曾在成功路径末尾才同步该数组，错误退出会丢掉
  它（#915）；现在 Ok/Err 两条路径都先同步再返回。

用例覆盖：empty single / empty general（no-op）、single 完整 dream
（双类别 refine、meta 更新、画像改写吸收 note、manifest 修复）、kept-profile
（画像未变、note 保留）、null profile、whitespace-only profile、rewrite 抛错
（catch + logger.warn）、画像写入失败与 note 删除失败（store 失败仍在 `run()`
内被 catch）、metadata-only（`isNoOp` 走 `!metaUpdated`）、200 文件窗口截断、
General 元合并后仍有两个存活项目（跨项目累加与两次元更新）、以及 #915 的
warning-then-error 格。

P1 的两个 store 故障用真实文件权限注入（`faults.profileReadOnly` /
`faults.notesReadOnly`）：生成器和 Rust 回放都在 seed 之后把 profile 文件改成
0444 或把 `UserIdentityNotes/` 改成 0555，失败点就在 TS `try` 内的
`upsertUserProfile` / `deleteMemoryEntries`，不靠改 store 实现。

生成与字节校验（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-run/generate.mts /tmp/edgeclaw-dream-run-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-run/generate.mts /tmp/edgeclaw-dream-run-2
cmp /tmp/edgeclaw-dream-run-1/oracle.json /tmp/edgeclaw-dream-run-2/oracle.json
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-run/verify.mts
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_dream_run
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_dream_run -- --include-ignored
```

`fixture_regenerates_identically` 是 `#[ignore]` 重生成守卫，会重跑生成器并
要求 `oracle.json` 逐字节一致。

## 负控

`negative.py` 用 `negative_process.mutating_sources` 锁定并恢复
`dream/run.rs`、`dream/runner.rs` 与 `dream/helpers.rs`，先校验 15 个变异锚点
恰好各出现一次（过期报 `STALE ANCHOR` 并退出 2，不冒充 SURVIVED），再对选中
变异的目标测试逐个跑未变异源码（任一无变异失败即拒绝判杀），逐格只跑绑定
`#[test]`，要求测试本身失败且日志出现该格专属 `#774 ...` 断言。编译失败、
无关 panic、缺专属消息都记 INVALID；每格恢复 baseline 字节；最后汇总，任一
非 KILLED 即非零退出。cargo 强制用调用方 `CARGO_TARGET_DIR` 并走
`run_owned` + `wait_for_slot`。

`run.rs` 与 `edgeclaw_dream_run.rs` 提交前不在 HEAD，`mutating_sources`
会按设计拒绝；本树只跑 `--check-anchors`，并用
`_tmp/20261026-issue-774/fix-r1/mutation_probe.py` 手工 apply/compile/restore
证明 15 个变异都能编译且在生产断言上被杀。全量由协调者在宿主提交后跑：

```sh
cd <tree>
export CARGO_TARGET_DIR=<tree>/_build/cargo-target
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-run/negative.py \
  _tmp/20261026-issue-774/negative-run
```

禁止影子 git、第二份构建目录或任何绕过“源码须等于 baseline”守卫的方式。
