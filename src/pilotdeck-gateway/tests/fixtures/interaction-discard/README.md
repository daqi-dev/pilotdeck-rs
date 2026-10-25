# #686 丢弃明示金样

`discard-probe.mts` 通过共享 `baseline_source_guard` 核验 cc7c4e6 源码树，直接 import 真实
`HookRuntime` / `LifecycleRuntime` / `AgentSession` / `SteerMailbox` / `TurnRunner` /
`AgentLoop` / `GatewayPermissionBus` / `GatewayElicitationBus`，不复制基线实现、不写基线、不用 socket。

从仓库根运行（stdout 先写新的证据文件，不覆盖 oracle）：

```sh
PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/interaction-discard/discard-probe.mts
```

`oracle.json` 是两次生成逐字节一致的输出（证据 `_tmp/20261025-issue-686/ts-probe/fixture-run{1,2}.json`，
sha256 均为 `5cff8ff5f2f5afb34798b28f375cd8d4f6ba78c88f33e0fe23003ef4d92ee776`）。它锁定四件事：

- `hooks`：TS 的 async pending 跨轮保留；两轮 dispatch 只有 `started/response`，没有丢弃提示。
- `steer`：未 drain 的条目在 close 时由 `TurnRunner` 逐项 yield `steer_unapplied {sessionId,turnId,itemId,reason:"turn_ended"}`，先于 `turn_completed`；没有聚合 count 提示。
- `permission`：turn 结束 `denySession` 前 pending=1、后 0；dispatch 无丢弃提示。
- `elicitation`：`rejectSession` 前后 pending 1→0，bus 无事件/提示通道。

Rust 侧不把 TS 的「hooks 无提示 / permission 无提示」当期望：按 #686 人定，Rust 在保持丢弃的同时发出
`interaction_pending_discarded`（立即日志 + 下一轮 `warning`）。形状由
`runtime/project/interaction_notice.rs` 的单测和 `runtime/project/*/tests.rs` 的逐字段断言锁定。
