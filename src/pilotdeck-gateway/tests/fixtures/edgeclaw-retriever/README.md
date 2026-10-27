# EdgeClaw ReasoningRetriever (A6.6-edgeclaw-10, #767)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/retrieval/reasoning-loop.ts:364-799`（`ReasoningRetriever.retrieve`、
  30 s `recallCache`、`resetTransientState`、`getRuntimeStats`）
- `service.ts:889-913,971-979`（`service.retrieve` / `retrieveContext` /
  `search` 三个薄包装）

`observe.mts` 直接 import 固定 TS 的 `ReasoningRetriever` 与
`MemoryRepository`，用真实 `FileMemoryStore` 在隔离 `HOME` 下写 single /
general 两套工作区与 global 根，再以 `../edgeclaw-llm/stub.mts`（127.0.0.1
随机端口）驱动 retrieve。`Date` 冻结在 `2026-10-22T00:00:00.000Z`，
`Math.random` 固定为 `0.123456789`；stub 逐个记录 method、url、小写头、body
原字节与 `at`。`generate.mts` 先 `bindSourceTree` 绑定提交，再对四个 TS 文件
做 `sourceClosureSha256`（含相对 import 闭包），把文件树（Rust 侧物化后打开
真实存储）、每步 result/stats/requests 和 M10 矩阵写进 `oracle.json`。

M10 矩阵另由 `observe.mts::collectMatrix` 用真实 TS 类 + 假 repository /
gated extractor 生成（缓存层与存储、HTTP 无关，gate 让 await 交错确定）：
空缓存 / 归一化命中 / 异键 / reset / TTL 29_999 与精确 30_000 边界 /
reset-during-await / 同键并发。Rust `tests/edgeclaw_retriever.rs` 的
`m10_*` 用例逐格重建这些状态，`recent` / `snapshot` 两个键分量另有独立用例。

生成两次并核字节（从仓库根目录运行；必须 `LANG=en_US.UTF-8` 且移走
`LC_ALL`，否则 Node 默认 locale 是 `und`）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-retriever/generate.mts /tmp/e767-1
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-retriever/generate.mts /tmp/e767-2
cmp /tmp/e767-1/oracle.json /tmp/e767-2/oracle.json
```

测试（`regenerates_identically` 必须带 `--include-ignored` 实跑；ICU 默认
locale 需与生成时一致）：

```sh
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 PDRS_BASELINE="$PDRS_BASELINE" \
  cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test edgeclaw_retriever -- --include-ignored
```

`negative.py` 用 `negative_process.{mutating_sources,run_owned,wait_for_slot}`
锁定并恢复产品源码。11 个变异体：缓存键漏 `recent` / `snapshot` / `mode`、
query 归一化失效、TTL 值 30_000→30_001、TTL 比较 `>`→`>=`、reset 不清、
await 后不 set、命中 `elapsedMs` 非 0、命中不写 stats、trace 拼接退回 UTF-8。脚本先跑一遍未变异
套件并要求通过，再逐格执行；单格失败不中止，最后打印
`KILLED`/`SURVIVED`/`INVALID` 汇总，任一非 KILLED 即退出 1。锚点在跑前
逐条核对，过期打印 `STALE ANCHOR` 并退出 2（不记 SURVIVED）；`could not
compile` 不算杀死。`mutating_sources` 在退出时逐字节恢复并与基线核对。

```sh
CARGO_TARGET_DIR=/abs/target python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-retriever/negative.py /tmp/e767-negative
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-retriever/negative.py --check-anchors
```

本 fixture 与 `retriever.rs` 在提交前不在 HEAD，`mutating_sources` 会按设计
拒跑；先 `--check-anchors`，完整 11 格由协调者在提交后的宿主执行。

`_tmp/20261026-issue-767/probe-m10.txt` 是 M10 的真实 TS 探针输出；
`probe-utf16.txt` 是孤代理码元（`\ud800`）query / mode 在真实 TS trace、
debug 与缓存命中路径上的逐码元输出，Rust
`utf16_lone_surrogate_preserved` 用同一组码元断言。
