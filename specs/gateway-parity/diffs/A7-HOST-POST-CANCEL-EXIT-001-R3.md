---
id: A7-HOST-POST-CANCEL-EXIT-001-R3
original: A7-HOST-POST-CANCEL-EXIT-001
---

## 类别

不可避免（有界期限的代价），向安全偏离。R2 列出的「覆盖不到」情况，经人选择维持现状并登记为已知偏离，不再作为待修缺陷。

## TS 依据

cc7c4e6 `src/agent/modules/transport/agentLoopSidecarClient.ts:931-958`：abort 只置 `cancelRequested` 并发送一次 cancel 帧，没有取消后计时器；`:547` 每次 `await iterator.next()` 只取一帧，消费方慢时整个生成器停住，恢复后按序交付。因此 TS 在「已取消 + 消费方慢」时无限等待，最终按序交付全部事件。

## Rust 行为

`src/pilotdeck-gateway/src/host/execution.rs` 的取消期限（`POST_CANCEL_EXIT_TIMEOUT` = 5 s，唯一绝对期限）到点时执行 `callbacks.shutdown(); events.clear();`。消费方停顿期间执行 future 不被 poll，墙钟期限照常推进。下列情况在到点时会丢掉合法 lifecycle 并报 `terminal without proper lifecycle` 或 Timeout（fail-closed，不会假成功）：应用回调需要多步才完成；会话通道 `mpsc::channel(1)` 前面还有非 final 事件；帧已在连接缓冲中但尚未 `receive()`；配置了 `execution_deadline` 时它在 `biased` 下先于 `join_next`。

## 范围

只在「用户已取消，且消费方停顿超过 5 s」时与 TS 不同：TS 最终报 success（或按序交付 error），Rust 报错。未取消、或消费方及时消费时行为一致。R1 的「唯一绝对期限，不重算、不额外武装」保持不变。四格矩阵（#590）中涉及「取消 + 慢消费方」的场景按本偏离判定。

## 机器判据

R2 登记的判据仍然有效（`live_stream_tests.rs` 的 `slow_consumer_*` 三条与 host 层 `completed_application_is_collected_before_the_deadline_exit`）；本更正不新增实现，只改变这几种情况的定性：从「待修缺陷（#643）」改为「已接受的已知偏离」。

## 批准记录

人批：2026-10-25「#643 选 A」，见 `state/decisions-20261025.md`「人定 · 2026-10-25 · #643 取消退出到点后的有界收尾：选 A」。仅覆盖上文「范围」所列情况。
