# #708 轮生命周期矩阵 TS 探针（steer 端口 × 交互事件端口）

`generate.mts` 以共享 `baseline_source_guard` 核验 cc7c4e6 源码树，直接 import `AgentSession`、`SteerMailbox`、`TurnRunner`、`AgentLoop`、`createSidecarAgentTurnCapabilities`、`createAgentEventBuffer`；只注入 stub loop / transcript 与 Promise 闸门，不复制被测实现，无 socket、模型调用或基线写入。stdout 保存到新证据文件后与 `oracle.json` 比较（两次生成已逐字节一致，`_tmp/20261025-issue-708/oracle-run{1,2}.json`）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck node \
  --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/turn-lifecycle-matrix/generate.mts
```

场景 ↔ 矩阵格（终版矩阵见 `specs/gateway-parity/turn-lifecycle-matrix.md`）：

- `A1_running_mailbox`（A-S1×E2/E3/E5）：运行中 mailbox 直打：insert、去重、turn_mismatch、cancelled tombstone、drain、offered too_late、claim/ack、drainOrClose 关闭、turn_closing、finish 后 no_active_turn。
- `A2_abort_gate`（A-S2×E2/E4）：abort 闸门只在 `AgentSession.steer`（status 检查），mailbox 与应用链本身无取消概念：abort 后 steer → no_active_turn，而已 drain 条目的 claim 照常落 claim 记录。
- `A2_apply_after_abort`（A-S2×E4 durable 前半段）：真实 TurnRunner+AgentLoop，abort 注入在 drain 与 apply 之间 → TS 仍完成 claim→durable→ack 并 yield steer_applied（`modelCalls=0`）。这是 Rust `TurnSteer::persist_and_apply` 取消检查（`interaction_steer.rs` `TurnSteer::check`）的差异证据，已登记 `specs/gateway-parity/diffs/A6.9-STEER-RUN-002.md`，不在本单内修。
- `A3_barrier_race`（A-S3×E1/E2）：insert 挂起中 finish+start 同 ID → 旧 enqueue accepted、新轮 drain 泄漏旧消息（A6.9-STEER-RUN-001 已登记差异，非 Rust 期望）。
- `A4_sequential`（A-S4/S5×E1/E2/E5）：真实 TurnRunner 顺序两轮：未 drain 的 steer 在 close 时 discard（记录 + steer_unapplied 先于 turn_completed）；新轮 mailbox 干净、旧 turnId → turn_mismatch；轮间 steer → no_active_turn。
- `A6_consumer_break`（A-S1×E6）：消费方在 loop 事件处停止迭代 → TurnRunner finally（TurnRunner.ts:442-446）仍执行 closeSteerMailbox，落 discard 记录；steer_unapplied 事件不再被消费方观察。
- `B2_event_buffer`（B-S1×E3）：`createAgentEventBuffer` 到达序保留、drain 清空、再 drain 空、晚到事件单独成批。
- `B5_unapplied_order`（B-S5×E4 尾段事件序）：成功路径 steer_unapplied 在 turn_completed 前；错误路径 agent_status → turn_failed → steer_unapplied → turn_completed（TurnRunner.ts:408-441）。

Rust 侧对照：`runtime/project/interaction_steer/tests.rs`、`runtime/turn/live_stream_tests.rs`、`runtime/project/interaction_host_tests.rs`、`runtime/project/factory_tests.rs`（逐格映射见矩阵文档）。交互端口的 wire 预留机制（首/中/尾释放）在 TS 无对应机制（单一 generator pump，`agentLoopSidecarClient.ts:379-399`），矩阵中按「不适用（对拍可观察交付顺序）」处理。
