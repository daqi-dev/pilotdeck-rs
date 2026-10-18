# Steer mailbox / turn dispatcher 金样

真实基线固定为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器直接 import `SteerMailbox`、`createSidecarDefaultModuleDispatcher`、`HostToolCheckpoint` 和 `TurnRunner`，不复制被测实现。前后调用共享 `_shared/baseline.mts` 检查固定提交、tracked 工作区干净及 committed blob/source SHA-256。每次运行创建独占 `mkdtemp` 工作目录，结束只删除本次目录；不接用户存储、模型或网关。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/steer/generate.mts /absolute/output-directory
cd src
cargo test --locked --offline -p pilotdeck-gateway --test steer
```

生成器与 Rust 回放均精确断言：15 组 mailbox / 192 个操作，18 组 dispatcher / 35 个操作，3 组真实 TurnRunner 场景。`oracle.json` 包含全部输入、逐步结果、持久化调用、授权快照、方法列表与 runner 回调顺序。存储故障和挂起通过注入端口制造，具体结果由真实 TS 产生；不依赖时间等待。

- Mailbox：身份不符、无活动轮、去重不替换、取消 tombstone、pending/offered/claimed、重复 claim/ack、close 后 enqueue/cancel、finish/restart、空串/孤立 UTF-16；另含 insert 与 close/drainOrClose/finish/restart、cancel 与 drain、claim 与 ack/restart 交错，以及 insert/cancel/claim/discard 写入失败后的队列恢复。
- Dispatcher：两个 drain 独立可选、缺省/错误 operation、异步端口失败、完整响应原样回传、忽略远端冒充 turn 和授权字段、字符串授权过滤、同 ID 后写覆盖及空列表保留、unknown/repeated apply、close callback 结果扩展字段。
- TurnRunner：用真实 `run()` + 假 loop / transcript 捕获 tracking、claim → durable、applied 后追加与 ack、重复 applied、不带 queueItemId 的 durable 消息；claim/durable 失败不 ack，未应用项保留。Rust 回放比较本次移植的回调和 tracking，不宣称实现了完整 TurnRunner。

Rust 另测多线程同 ID 入队仅产生一条 insert、竞争 drain 单次投递、丢弃等待 future 后仍完成已排队持久化、durable 失败不授予权限且可重试、旧工具上下文不被新授权回写。

接口边界：mailbox 输入遵守 TS `AgentSteerMessage`（字符串 itemId/paths、canonical message）；dispatcher 的宿主 callback 返回对象数组/对象，并对 `allowedReadFiles` 做真实 TS 的字符串过滤。`SteerMailbox` 的异步变更方法须在 Tokio runtime 内调用，调用即排队，丢弃返回 future 不撤回持久化。同步边界不等待 recorder；调用方按 TS 生命周期管理 start/finish。`Handler::persist_and_apply_steer` 提供 claim → durable → grant → append/ack 的接线入口；turn module 不接受远端 claim/ack/grant 操作。persist_compaction 留给 A6.6-pr2，manifest 合成留给 A6.9，整轮 sidecar event 消费留后续装配。
