---
id: A7-W6-REWIND-MAINTENANCE-LOCK-001
---

## 类别

有意改进：Rust 的 rewind_files 用 maintenance reservation 串行化并发 rewind 请求，并在 rewind IO 期间阻止新 turn；TS 对并发 rewind 无独立守卫，只检查 hasActiveTurn。不放宽判分。

## TS 依据

固定基线 cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，Node v22.22.0 / ICU 78.2。
`src/gateway/tools/rewindFiles.ts`（或 SessionRouter 中的对应路由）：入口只检查 `hasActiveTurn`；无并发 rewind 互斥机制，第二个并发 rewind 请求不会在入口被拒绝。TS 不持有 maintenance 类似的预约标志，也不在 rewind IO 执行期间阻止新 turn 提交。

## Rust 行为

`src/pilotdeck-gateway/src/native_process/methods/tools.rs` 的 rewind_files 路由：

1. **并发 rewind 拒绝**：调用 `owner.begin_maintenance(&session_key)`；`runtime/session/admission.rs::begin_maintenance` 在 `maintenance` 集合已含该 session key 时返回 false，路由返回 `SESSION_BUSY / "A rewind is already in progress."`。TS 无此守卫，第二个并发 rewind 可进入执行。
2. **IO 期间阻止新 turn**：maintenance reservation 持续到 rewind_files 返回（guard 跨越整个 IO 调用）；`admission.begin_turn` 检查 maintenance 集合，期间 begin_turn 被拒绝。TS 的 hasActiveTurn 检查仅阻止 turn 在 turn 活跃期间开始，不反向阻止 turn 在 rewind 期间开始。
3. **`has_active_turn` 分支已删除**：`begin_maintenance` 在 `turns.contains_key` 为真时必返回 false，成功拿到 guard 后 has_active_turn 恒 false，该分支为死代码，已删除（fix-r3）。SESSION_BUSY 仍由 maintenance 守卫（情形 1）和 maintenance 预约语义（情形 2）覆盖；文案 "A rewind is already in progress." 由情形 1 返回。

## 范围

本包 native isolated 生产入口的 rewind_files 路由（`native_process/methods/tools.rs`）及 `runtime/session/admission.rs` 的 maintenance 预约机制。不涉及 TS 的 hasActiveTurn 语义、proxy 分发、transcript 格式或文件恢复逻辑。

## 机器判据

互斥逻辑的底层测试位于 `runtime/session/tests.rs:242-254`（`keys_preserve_utf16_and_maintenance_shares_turn_admission`）：第一个 `begin_maintenance` 成功、第二个同 session key 的 `begin_maintenance` 被拒、`begin_turn` 在 maintenance 期间被阻。该测试覆盖 admission 层语义，不在 native_process 路由层。

native_process 路由层的 SESSION_BUSY 路径（情形 1）和文案 "A rewind is already in progress."（来源：`methods/tools.rs:185`）目前无独立断言；前置于 P2-跟进待补。

覆盖账本：`specs/gateway-coverage/ledger.json` 中 `a51.rewind_files` 的 proposed_scenarios（success / error / unconfigured）不含 SESSION_BUSY；因情形 2（turn 被 rewind 阻断）在 TS 无对等行为，对拍不计入该组。情形 1 的 SESSION_BUSY 由本条目覆盖，不视为假 FAIL。

## 批准记录

2026-11-04，AI 代批，待人复核。依据：fix-r2 评审 P1-1（dead code）与 P1-2（缺 accepted-diffs 条目）；maintenance 锁语义为已实现的有意改进，与 TS 无对应，向安全侧偏离。本条不自动增加判分豁免；人复核后以持久决策指针登记，不以本轮测试替代实效签字。
