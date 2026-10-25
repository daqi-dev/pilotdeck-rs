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
