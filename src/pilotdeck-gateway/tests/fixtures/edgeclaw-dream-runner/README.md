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

`negative.py` 用 `negative_process.mutating_sources` 锁定并恢复 `runner.rs`，
先校验 15 个变异锚点恰好各出现一次（过期报 `STALE ANCHOR` 并退出 2，不冒充
SURVIVED），再跑未变异套件要求退出 0；逐格只跑该变异绑定的 `#[test]`，要求
测试失败且日志出现该格专属的 `#773 ...` 断言消息。编译失败、无关 panic、
缺专属消息都记 INVALID。每格跑完恢复 baseline 字节，最后再核一次；逐项汇总
KILLED/SURVIVED/INVALID，任一非 KILLED 即非零退出。cargo 强制使用调用方
`CARGO_TARGET_DIR` 并走 `run_owned` + `wait_for_slot`。

新文件在提交前不在 HEAD，`mutating_sources` 会按设计拒绝；本树只跑
`python3 .../negative.py --check-anchors`，全量由协调者在宿主提交后跑：

```sh
cd <tree>
export CARGO_TARGET_DIR=<tree>/_build/cargo-target
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/negative.py \
  _tmp/20261026-issue-773/negative-run
```

禁止影子 git、第二份构建目录或任何绕过“源码须等于 baseline”守卫的方式。
