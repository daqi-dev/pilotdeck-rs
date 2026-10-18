# Durable recorder TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测入口 `src/agent/session/AgentSessionEventRecorder.ts` 的 `startTurn`、`recordModelRequest`、`recordModelEvent`、`recordToolCalls`、`recordToolResults`；同批源码（含 `durableModelInvokerPort.ts`、`durableToolPort.ts`、writer/runtime/域校验/JSONL 持久化）的 SHA-256 记在 `oracle.json`。所有记录由真实 `AgentSessionEventRecorder` + `JsonlTranscriptWriter` 写入每次运行独占的临时会话目录，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/recorder/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/recorder
```

生成器先断基线的已跟踪文件无改动，并按 `cc7c4e6` 的提交内容复算每个来源的 `sourceSha256`。每次运行在输出目录下 `mkdtemp` 一个 `.generate-*` 临时目录，只清理本次创建的目录；会话 transcript 与步骤返回值都取自真实 TS，UUID/目录归一化后写入 `oracle.json`。

`oracle.json.scenarios[]`：`name`、`steps[]`（输入 + TS 返回值或 `{error:{name,code?,message}}`）、`files`（会话目录下全部文件原文）。覆盖：

| 场景 | 覆盖 |
|---|---|
| model | 首次请求开 step 1；事件逐条落盘；第二次请求先写 `step_completed` 再开 step 2；request 中的 `raw` 保留 |
| tools | `tool_call` 先于执行、`tool_result` 后于执行；空结果集也会关闭 step；`step_completed` 的序号与 parent 链 |
| raw | `model_stream_event`、`tool_call`、`tool_result` 递归剔除 `raw`（含数组与嵌套对象），request 不剔除 |
| errors | 未 start 的 model/tool 调用；重复 start；未开 step；`tool_result_without_call`；`duplicate_tool_call`；`step_has_open_tool_calls`（前一条 result 已落盘）；null call/result 的 V8 TypeError；失败不发布 sequence |
| restart | 重开 writer 并 `restoreState` 后继续，序号/parent 链从持久化尾部续写 |

Rust 默认测试 `real_ts_scenarios_match_outcomes_and_bytes` 逐场景重放，把步骤结果与整份 transcript 文件序列化后与 oracle 逐字节比较。归一化只有两条：场景目录 → `<dir>`；v4 UUID → `<id-N>`（按整份场景文本首次出现顺序）。

交叉测试 `real_ts_and_rust_continue_each_others_transcripts` 由 `generate.mts <dir> --cross-ts-write` / `--cross-ts-verify` 驱动：

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway --test recorder -- --include-ignored
```

TS 写 14 条 → Rust restore 后续写 7 条 → TS restore 校验序号/parent 链后再写 7 条；反向 Rust 写 14 条 → TS 校验并续写 7 条 → Rust 再续写。两侧都断言序号连续、parent 链完整、`raw` 已被 TS/Rust 各自剔除。

范围：只证明 durable model/tool 记录与 A6.2 transcript 存储同格式；未接 host、`ProjectRuntimeRegistry` 或 gateway method，不宣称宿主侧 model/tool port 已本地化。compaction/question/permission/inbox 生命周期与 `completeTurn` 留给后续 PR。
