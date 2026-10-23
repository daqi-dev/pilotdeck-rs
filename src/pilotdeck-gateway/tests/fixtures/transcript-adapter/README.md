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
| node-only-* | 历史命名保留，三个场景与其他场景一样逐字节对拍：turn_started 成功落盘、畸形 accepted_input 恢复成功、null turn_result 列表抛 TS TypeError |
| domain-* | 真实 TS 域日志经过 recordEntry 校验、store 调用与 JSONL 落盘；另测失败不发布状态、恢复后继续 step、域校验失败、全日志 envelope 校验先于域校验 |

`generate.mts <dir> --cross-ts-write` / `--cross-ts-verify` 由 Rust 忽略测试 `real_ts_and_rust_continue_each_others_transcripts` 调用：TS 写主记录与 sidechain 并留下坏尾 → Rust restore 后续写主记录、恢复 sidechain writer 后续写 → TS restore 校验序号、parent 链与 sidechain 后继续写 → Rust 再读。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway transcript_adapter -- --include-ignored
```

范围：只证明异步 transcript 适配器契约；未接 host、ProjectRuntimeRegistry 或 gateway method，不宣称本地化。替换事务、fork 复制、file-history/tool-result 制品能力与 NativeSessionStorageAdapter 校验留给后续 PR。

#222：域用例输入与 `../session-domain/cases.mts` 共用；完整校验范围见其 README。尚未移植的恢复/列表投影形状返回 `ProjectionUnavailable`（A7.5），不能视为 TS 一致或已完成投影。

修复轮 1（#562）：`restore-message-*` 由真实 restore/hydrate 生成，覆盖三类 message 条目的 20 种形状：缺失/null/原始值 message，非数组 content，null block、原始值 block、空数组，以及 tool_result/tool_result_reference 的 `.some` 短路。比较完整输出与 TypeError 文案；不把 canonical-message 形状校验代替 JS reducer 语义。TS 来源 `WebHistoryProjections.ts:79,321-324`，另加一条 reducer 全日志执行顺序用例，总计新增 61 场景（336→397）。WebHistory、SessionProjectionDriver 与 BuiltinSessionProjections 源码也纳入逐文件溯源守卫。

修复负控：在仓根运行 `python3 src/pilotdeck-gateway/tests/fixtures/session-domain/fix1-negative.py`。脚本核启动器 target、逐 cargo 核 slot、对子进程组设期限，实际修改产品源码后验证行为失败并在 finally 恢复。除消息错误、短路和原始值 block 外，还把 restore 的锁移到读盘之后，要求确定性的锁范围回归测试转红。

修复轮 2（#562）：生成器与 Rust 测试均精确断言 397 场景；增删场景时须同步修改两处计数与本 README，防止金样再生或合并时覆盖静默缩水。现有 61 条 restore-message 场景继续读取 writer 状态、追加 metadata、读取磁盘并再次 restore；对齐 SessionRuntime.ts:106-115 与 ProjectSessionStorage.ts:337-346 的 envelope→domain→状态安装→hydrate 顺序，hydrate 失败后仍延续原 sequence 与 parent 链。
