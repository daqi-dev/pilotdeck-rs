---
id: D1-NATIVE-PENDING-AUTOWAKE-001
---

## 类别

有意改进：恢复原生会话时自动继续持久 pending，避免待处理记录永久占用会话。

## TS 依据

固定 cc7c4e6 的 AgentHandle.ts:52–55 constructor 不自动起 pump；startQueuedPump 由 followup 等显式调用，manager coldResume 通过 followup 唤醒。runQueuedPump:307–335 只消费 iterator，不把宿主生命周期事件回流为第二份 AgentLoop 流。

## Rust 行为

`runtime/turn/live_session.rs` 与 `native_process/agent_continuation.rs` 在发布真实会话 owner 后触发 pending pump；失败发布保持 rollback，使用代际身份和取消后收尾守卫。父 followup 只 drain，不重复投递已记录的 host lifecycle。

## 范围

原生 CLI 的父会话已落盘 pending 恢复；warm followup 的去回流属于旧行为修复。自动恢复部分单列差异，不扩大阶段判据或宣称 TS 自动恢复等价。

## 机器判据

r63 的真实发布/rollback、slot身份守卫已通过；parent_followup_drains_host_events_without_refeeding_the_sidecar_buffer 检验 drain 到 SessionEnd/EOF 且 sidecar 缓冲为空。实际 warm/cold/restart probe 待修复新bin复验，先前 StreamIntegrity 拒绝原件保留，不能以此文宣称通过。

## 批准记录

AI 代批，待人复核；未批准，非已接受差异。依据完成CLI与机器恢复能力的授权作可撤回的实现判断，不改冻结计分。
