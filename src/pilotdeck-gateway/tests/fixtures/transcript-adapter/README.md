# Async transcript adapter TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测入口 `src/gateway/storage/AsyncTranscriptStorageAdapter.ts`（连同 writer/runtime/校验/存储/读取源码的 SHA-256 记在 `oracle.json`）。所有结果都由真实 `createGatewayAsyncTranscriptStorageAdapter` 产生，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/transcript-adapter/generate.mts src/pilotdeck-gateway/tests/fixtures/transcript-adapter
```

生成器的 host store（`fixtureStore`）是 TS 默认 provider 套上异步契约：append 走真实 `JsonlSessionPersistence`，read 走真实 `readTranscript`，另记调用日志并按序号注入失败（写前失败 `failBefore`、写后失败 `failAfter`、读失败 `failRead`），只暴露场景声明的可选能力（has/delete/deleteSession/replace/list）。Rust 侧 `FaultStore` 以同样语义包住 `JsonlTranscriptStore`。

`oracle.json.scenarios[]`：`store`（能力与注入）、`initialFiles`（相对 chatDir 的起始文件）、`chatDir`、`steps[]`（输入 + TS 结果）、`calls[]`（store 收到的每次调用：key 与 entry/entries/options）、`files`（场景目录下全部文件原文）。每个场景生成时逐步断言结果类别（open:N / restored / read / state / error:<code|name> / list / concurrent…），防止步骤落空。

Rust 默认测试 `real_ts_scenarios_match_outcomes_calls_and_bytes` 逐场景重放，把整份场景（步骤结果 + 调用日志 + 落盘文件）序列化后与 oracle 逐字节比较。两条归一化：
- **场景目录 → `<dir>`**：项目根固定为 `/pdrs-fixture/project`（只作 key 与项目 id，不落盘），pilotHome 在场景目录下；
- **v4 UUID → `<id-N>`**：TS 适配器不暴露 uuid 注入，entryId 按整份场景序列化文本中的首次出现顺序编号，Rust 用同一正则同一顺序。
`transcript_line_invalid` 诊断不比 V8 的 JSON.parse 文案，其余诊断文案都比。

| 场景 | 覆盖 |
|---|---|
| normal / restart / legacy | 新会话、空 metadata/detail 省略、重开 restore 续 sequence 与 parent 链；不 restore 直接写会从 1 重来（TS 行为），之后 restore 报 `sequence_not_increasing`；无 entryId 的旧数据 parent 为 null，他会话条目不过滤 |
| append-failure / append-uncertain / concurrent | 写前失败不发布（下一条复用同一 sequence 与 parent）；写后失败同样不发布，盘上出现重复 sequence，重开 restore 报错；四条并发入队、第二条失败时其余按入队顺序提交 |
| bad-tail | 中间坏形状行与无 LF 坏尾：restore 跳过并给诊断，下一次 append 保留残片、先补 LF |
| restore-* | `validateSessionEventLog` 每个错误分支（0、-0、小数、超安全整数、空白/null entryId、数字 parent、自引用、重复 id） |
| record-entry / restore-state | `appendRecorded` 与 `restoreState` 的每个错误分支，失败不改状态 |
| closed / sidechain | close 后写入不到 store；sidechain key 带父 sessionId、路径净化、sidechain 的 sidechain 用未净化默认布局、每次 forSubagent 都从 0 开始 |
| domain | 无 turn 状态下 durable message 的 `message` 为 undefined/null 抛 TypeError（V8 文案），legacy turn_result 通过 |
| no-capabilities / replace-delete / read-limit | 可选能力缺省；replace 不重置 writer 序号；delete/deleteSession；`maxBytes` |
| list / list-read-failure | 过滤（空、错误诊断、无摘要）、title/aiTitle/prompt 摘要优先级、metadata 合并（title 遇 null 保留、其它键 spread）、Date.parse 失败为 null、稳定降序、offset/limit 的负数/小数/0；读失败整体拒绝 |
| node-only-* | 声明的缺口：turn_started 域校验、畸形条目 restore 投影、畸形 turn_result 列表投影。Rust 必须在第一处与 TS 分歧的步骤返回 `NodeHostRequired`，不得伪造结果 |

`generate.mts <dir> --cross-ts-write` / `--cross-ts-verify` 由 Rust 忽略测试 `real_ts_and_rust_continue_each_others_transcripts` 调用：TS 写主记录与 sidechain 并留下坏尾 → Rust restore 后续写主记录、恢复 sidechain writer 后续写 → TS restore 校验序号、parent 链与 sidechain 后继续写 → Rust 再读。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway transcript_adapter -- --include-ignored
```

范围：只证明异步 transcript 适配器契约；未接 host、ProjectRuntimeRegistry 或 gateway method，不宣称本地化。替换事务、fork 复制、file-history/tool-result 制品能力与 NativeSessionStorageAdapter 校验留给后续 PR。
