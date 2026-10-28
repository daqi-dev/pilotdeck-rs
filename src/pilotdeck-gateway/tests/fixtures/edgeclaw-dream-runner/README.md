# EdgeClaw DreamRewriteRunner sub-flows (A6.6-edgeclaw-16, #773)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`
的 `core/review/dream-review.ts:532-1025`：`runCategoryDream`（543-798）、
`mergeGeneralProjectMetas`（800-913）、`runProjectMetaReview`（915-1024）。
外层 `run()`（1026-1492）属于后续 `dream/run.rs` 包。

`observe.mts` 直接 import 该提交的 `DreamRewriteRunner` 与 `MemoryRepository`，
用真实 SQLite + `FileMemoryStore` 造 stage 仓（single / general 两种），再用
脚本化 `LlmMemoryExtractor` 桩回放四个 LLM 方法。`HOME` 隔离到临时目录，
时钟冻结在 `2026-10-22T00:00:00.000Z`，`Math.random` 按固定序列消费；每次
`mutation()` 的随机值与 `Date.now()` 都进入金样。

每个用例记录：

- `expected.input`：从仓库查询出的 entries / records / projectMeta（仅
  `runCategoryDream`），Rust 侧用同一查询复算并逐值比较；
- `expected.result`：方法返回值；
- `expected.trace`：完整 trace（steps、details、mutations、promptDebug）；
- `expected.calls`：桩收到的每个 LLM 输入；
- `expected.logs`：runner 经 `logger.warn` 发出的日志（`dream-review.ts:838`，
  其余级别也记录，防止额外调用被静默丢弃）；
- `expected.tree`：操作后的内存文件树（相对路径 + 内容）。

绝对路径统一替换为 `$ROOT`，因此金样可在任意工作树重放。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/generate.mts /tmp/edgeclaw-dream-runner-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/generate.mts /tmp/edgeclaw-dream-runner-2
cmp /tmp/edgeclaw-dream-runner-1/oracle.json /tmp/edgeclaw-dream-runner-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_dream_runner
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_dream_runner -- --include-ignored
```

`fixture_regenerates_identically` 是 `#[ignore]` 重生成守卫，会重跑生成器并
要求 `oracle.json` 逐字节一致。

## 负控

`negative.py` 用 `negative_process.mutating_sources` 锁定并恢复
`dream/runner.rs`、`_shared/run_scratch.mjs` 与 `observe.mts`，先校验 26 个
变异锚点恰好各出现一次（过期报 `STALE ANCHOR` 并退出 2，不冒充
SURVIVED），再对**本次选中变异的目测试去重**逐个跑未变异源码，任一目标测试
未通过即拒绝判杀并以 1 退出；逐格只跑该变异绑定的 `#[test]`，要求测试本身
失败（`test <name> ... FAILED`）且日志出现该格专属的断言消息。
编译失败、无关 panic、缺专属消息都记 INVALID。每格跑完恢复 baseline 字节，
最后再核一次；逐项汇总 KILLED/SURVIVED/INVALID，任一非 KILLED 即非零退出。
cargo 强制使用调用方 `CARGO_TARGET_DIR` 并走 `run_owned` + `wait_for_slot`。

其中 `merge-error-empty-message` / `merge-error-empty-name` /
`merge-error-lossy-message` 分别绑定 fix-r2 的三个
`merge-plan-error-*` 金样，覆盖 `Error.prototype.toString()` 的空 message、
空 name 与孤代理 UTF-16 边界。

fix-r3 新增 `category-missing-record`（两个 header、一个 record）与
`scratch_root_is_run_exclusive` 两个目标：`plan-output-missing-record-sync` /
`plan-dropped-kv-recomputed` 杀 trace 同步与 push-time KV；`run-scratch-fixed-root` /
`observe-fixed-scratch` 杀固定 stage 路径回归。生成器每次 `collect()` 在
`PDRS_RUNNER_SCRATCH`（或 `tmpdir()`）下 `mkdtemp` 独占根，删除复用
`_shared/generator_guard.mjs` 的 `assertInside` / `removeOwned`。

#915 新增 `category-refine-error`（两个合法 cluster：第一组缺 record 追加
warning，第二组 refine 抛错）与 `plan-output-error-sync`：变异把同步包进
`if loop_result.is_ok()`，目标测试在 `#915 plan-output warning
category-refine-error` 上失败。该变异只改 `runner.rs`，而本单的产品源码相对
HEAD 未变，所以 `mutating_sources` 可以在未提交的本树直接跑；测试与金样是
工作树改动，由同一次 cargo 编译读取：

```sh
cd <tree>
export CARGO_TARGET_DIR=<tree>/_build/cargo-target
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/negative.py \
  _tmp/20261026-issue-915/negative-run
```

fix-r1 / fix-r2 / fix-r3 的产品改动在提交前与 HEAD 不一致，`mutating_sources`
会按设计拒绝；那几轮只跑 `--check-anchors` 并用手工 apply/restore 预检，全量由
协调者在宿主提交后跑。

禁止影子 git、第二份构建目录或任何绕过“源码须等于 baseline”守卫的方式。
