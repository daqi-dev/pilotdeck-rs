# EdgeClaw retrieval-side LLM calls (#765 / A6.6-edgeclaw-8)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`：

- `core/skills/llm-extraction.ts:2877-2915`（`decideFileMemoryRoute`）
- `core/skills/llm-extraction.ts:2917-3016`（`selectRecallProject`）
- `core/skills/llm-extraction.ts:3126-3178`（`selectFileManifestEntries`）
- `core/skills/llm-extraction.ts:1623-1637`（`recallProjectSourcePriority` /
  `chooseBestRecallProjectFallback`）
- `core/skills/llm-extraction.ts:213-215`（三个 45 s 默认超时）

`observe.mts` 直接 import 固定提交的 `LlmMemoryExtractor`，复用 #763 的
`../edgeclaw-llm/stub.mts` 在 127.0.0.1 随机端口回放 `chat` 响应，并记录每个
请求的 method、url、小写头与 body 原字节。`HOME` 隔离，`Date.now` 冻结在
`2026-10-22T00:00:00.000Z`，`Math.random` 固定为 `0.25`。每个 case 的
`expected` 含 `result`、`requests`、`debug`，全部来自真实 TS 执行。

覆盖 41 个 case：

- decide：user/project/mix/none、大小写不敏感 route 拒绝、`{bad}`、无 JSON、
  空响应、孤立代理、最近消息 `slice(-4)` 与 220 截断。
- select：命中、模型 id 前后空白、越权 id（allowEmpty true/false）、空
  shortlist 零请求、畸形 JSON 回退、fallback 的 exact/score/source
  priority/`localeCompare` 四层排序、description/matched_text 180、reason 220 截断、
  最近消息。
- manifest：正常/单串/非数组/缺字段/空数组/畸形 JSON、project null、
  空 manifest、`limit` absent/0/2.9/9、200 条上限、description 200 截断、
  project description 180 截断、最近消息。

生成与验证：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall/verify.mts
```

Rust 侧：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway \
  --test edgeclaw_llm_recall
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_llm_recall -- --include-ignored
```

`#[ignore] fn regenerates_identically()` 重跑 `generate.mts` 并逐字节比较
`oracle.json`。

## 负控

`negative.py` 有 14 个变异体，使用 `(name, path, before, after, test)` 表供
`anchor_selfcheck.py` 静态检查；每个变异都要求编译成功，并命中该 case 绑定的
`#765 result/requests <case>` 断言，否则记 INVALID。执行器逐项跑完再汇总
`KILLED / SURVIVED / INVALID`，任一非 KILLED 退出 1；stale anchor 退出 2。
变异在 `negative_process.mutating_sources` 内进行，cargo 走 `run_owned` +
`wait_for_slot`，`CARGO_TARGET_DIR` 必须由调用方传入。

`recall.rs` 是新增文件，提交前不在 HEAD，`mutating_sources` 会按设计拒绝
运行；本树只跑 `--check-anchors`。提交后由协调者在宿主执行全量 14 个变异体：

```sh
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall/negative.py \
  /tmp/edgeclaw-recall-negative
```
