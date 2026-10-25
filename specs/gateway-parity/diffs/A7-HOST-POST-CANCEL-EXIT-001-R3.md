---
id: A7-HOST-POST-CANCEL-EXIT-001-R3
original: A7-HOST-POST-CANCEL-EXIT-001
---

## 类别

不可避免（有界期限的代价），向安全偏离。R2 列出的「覆盖不到」情况，经人选择维持现状并登记为已知偏离，不再作为待修缺陷。

## TS 依据

cc7c4e6 `src/agent/modules/transport/agentLoopSidecarClient.ts:931-958`：abort 只置 `cancelRequested` 并发送一次 cancel 帧，没有取消后计时器；`:547` 每次 `await iterator.next()` 只取一帧，消费方慢时整个生成器停住，恢复后按序交付。因此 TS 在「已取消 + 消费方慢」时无限等待，最终按序交付全部事件。

## Rust 行为

`src/pilotdeck-gateway/src/host/execution.rs` 的取消期限（`POST_CANCEL_EXIT_TIMEOUT` = 5 s，唯一绝对期限）到点时执行 `callbacks.shutdown(); events.clear();`。消费方停顿期间执行 future 不被 poll，墙钟期限照常推进。下列情况在到点时会丢掉合法 lifecycle 并报 `terminal without proper lifecycle` 或 Timeout（fail-closed，不会假成功）：应用回调需要多步才完成；会话通道 `mpsc::channel(1)` 前面还有非 final 事件；帧已在连接缓冲中但尚未 `receive()`；配置了 `execution_deadline` 时它在 `biased` 下先于 `join_next`；重连内层的 `select!` 没有调整取件顺序，期限照旧可能先于已完成的应用被选中。以上五种即 R2「覆盖不到」清单中的全部行为项。R2 清单另有一条测试项（慢消费方三个用例假定帧在第二次 `next()` 时已入队，CI 慢时会偶发失败）不属于行为偏离，由分支 `claude/fix-slow-consumer-flake` 给这三个用例补确定性同步点处理，不随 #643 关闭。

## 范围

只涉及「取消已被观察到」之后：期限是从观察到取消起算的 5 s 墙钟（`host/execution.rs` 的 `POST_CANCEL_EXIT_TIMEOUT`，R1 的唯一绝对期限），与消费方停顿多久无关。期限到点时若仍有已收到、但尚未交付的合法 lifecycle，Rust 报错，TS 则无限等待并最终按序交付。造成「到点时仍未交付」的原因限于「Rust 行为」所列五种情况（回调等待锁或真实 I/O 即第一种「应用回调需要多步才完成」），它们都是 R1 有界期限的直接后果，本更正把这五种定性为已接受的已知偏离，不扩大到其他情况。R2 已修复的那一种情况（期限到点时应用已完成、只差收集）会先收集再退出，与 TS 一致，判据 `slow_consumer_does_not_spend_the_cancel_exit_grace`（已取消、停顿 6 s 仍得 success）。未取消的回合不受本条影响。四格矩阵（#590）中涉及取消的场景按本偏离判定。

## 机器判据

R2 登记的判据仍然有效（`live_stream_tests.rs` 的 `slow_consumer_*` 三条与 host 层 `completed_application_is_collected_before_the_deadline_exit`）；本更正不新增实现，只改变这几种情况的定性：从「待修缺陷（#643）」改为「已接受的已知偏离」。

## 批准记录

人批：2026-10-25「#643 选 A」，见 `state/decisions-20261025.md`「人定 · 2026-10-25 · #643 取消退出到点后的有界收尾：选 A」。仅覆盖上文「范围」所列情况。
