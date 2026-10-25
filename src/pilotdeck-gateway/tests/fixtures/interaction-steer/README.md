# Steer session / factory 真实绑定

`probe.mts` 以共享 `baseline_source_guard` 核验 cc7c4e6 源码树，直接 import `AgentSession` 与 `SteerMailbox`；注入 runner 只驱动真实 session 提供的 mailbox callbacks，不复制 mailbox 实现。无 socket、模型调用或基线写入。stdout 保存到新证据文件后与 oracle 比较：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/tests/fixtures/interaction-steer/probe.mts
cd src
cargo test -p pilotdeck-gateway --lib interaction_ -- --test-threads=4
cargo test -p pilotdeck-gateway --test steer -- --test-threads=4
```

`trace` 与 `mutations` 全量比较 enqueue 去重、取消 tombstone、drain、claim/ack、drainOrClose、close；`difference` 单独保存 TS 同 ID restart 泄漏旧 insert 的实测，不作为 Rust 期望。差异见 `specs/gateway-parity/diffs/A6.9-STEER-RUN-001.md`，未批准。

宿主通过 `Resource::InteractionSteer → bind_session(recorder) → start_turn(..., router)` 接入；保留项目 lease，event 消费使用返回的 `TurnSteer::persist_and_apply`，退出调用 `close`（包含 finish），session 退出调用 `dispose`。旧句柄不能重选新 run；同 session 必须等待旧轮清理成功才可再 start。dispatcher 的 dispose 仍不拥有 session mailbox。本包提供这些真实端口，不代替 #589/#590 的完整 LiveSession/sidecar 消费装配；真实 provider 效果待人工验收。

`cancel-probe.mts` 直接运行真实 AgentSession + TurnRunner + AgentLoop + SteerMailbox，覆盖 abort 后两个 drain，以及 claim → durable 成功 → abort → grant/ack；首个 steer_applied 后退出，断言 modelCalls=0。运行同上 probe 命令换为 cancel-probe，stdout 与 `cancel-oracle.json` 比较。Rust 取消 drain 测试逐值比较该 oracle，持久取消测试断言原消息已追加、授权已授予且 discarded=[]。

宿主 recorder（inbox mutation 与 transcript）必须有界；本模块不在 durable write 仍运行时提前释放 lease。后续 #589/#590 装配须提供 recorder 的 I/O deadline。
