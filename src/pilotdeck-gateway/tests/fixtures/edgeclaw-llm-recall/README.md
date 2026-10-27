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
`expected` 含 `result`、`requests`、`debug`、`warnings` 与 `events`，全部来自
真实 TS 执行；`events` 记录 `debugTrace` 与 `logger.warn` 的调用顺序。

覆盖 51 个 case：

- decide：user/project/mix/none、大小写不敏感 route 拒绝、`{bad}`、无 JSON、
  空响应、孤立代理、最近消息 `slice(-4)` 与 220 截断、空模型配置的 warning。
- select：命中、模型 id 前后空白、越权 id（allowEmpty true/false）、空
  shortlist 零请求、畸形 JSON 回退、fallback 的 exact/score/source
  priority/`localeCompare` 四层排序、JS 数值语义（`-0`/`+0` 相等、NaN、Infinity）、
  description/matched_text 180、reason 220 截断、最近消息、空模型配置的 warning。
- manifest：正常/单串/非数组/缺字段/空数组/畸形 JSON、project null、
  空 manifest、`limit` absent/0/2.9/9、200 条上限、description 200 截断、
  project description 180 截断、最近消息、空模型配置的 warning。

`spec.input` 用 `$number` / `$undefined` 哨兵编码，避免 JSON 序列化把 `-0`
变成 `0`、把 NaN 变成 null；Rust 回放先解码再调用移植。

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

`negative.py` 有 22 个变异体，使用 `(name, path, before, after, test)` 表供
`anchor_selfcheck.py` 静态检查；每个变异都要求编译成功，并命中绑定测试里的
专属消息（`#765 ...`），否则记 INVALID。执行器逐项跑完再汇总
`KILLED / SURVIVED / INVALID`，任一非 KILLED 退出 1；stale anchor 退出 2。
变异在 `negative_process.mutating_sources` 内进行，cargo 走 `run_owned` +
`wait_for_slot`，`CARGO_TARGET_DIR` 必须由调用方传入。`recall.rs` 与
`transport.rs` 都在变异路径内。

这两个文件在本次提交前不在 HEAD（`recall.rs` 新增、`transport.rs` 修改），
`mutating_sources` 会按设计拒绝运行；本树只跑 `--check-anchors` 与逐变异
编译检查。提交后由协调者在宿主执行全量 22 个变异体：

```sh
CARGO_TARGET_DIR="$CARGO_TARGET_DIR" python3 \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall/negative.py \
  /tmp/edgeclaw-recall-negative
```
