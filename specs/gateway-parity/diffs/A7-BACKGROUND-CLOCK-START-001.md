---
id: A7-BACKGROUND-CLOCK-START-001
---

## 类别

bug 修复：#804 的 start Clock panic 泄漏预留及毒化 Mutex；B2 的 TS 基线自身存在 dispose 挂起。仅 B1/B2，不宣布整个 #804 或 A7 完成。

## 基线依据

cc7c4e6 的 `src/task/runtime/BackgroundTaskRuntime.ts:239–252` finally 释放 activeStarts；:267 首次 now 抛错时无 entry；:319 同步 spawn 失败后的 now 抛错留下 failed entry 和未兑现 done。真实 TS 探针及固定来源哈希见 `src/pilotdeck-gateway/tests/fixtures/background-clock/{generate.mts,oracle.json}`；本轮原始 stdout 为 `_tmp/20261026-issue-804b/ts-probe.log`。B1 可再准入且三个 dispose 均结束；B2 可再准入，但三个 dispose 在所有可完成的注入操作排空后仍 pending。

## Rust 行为

`runtime/background.rs` 的 StartReservation 在任何 Clock 前持有两个计数；初始 Clock 在全局锁外运行。B1/B2 panic 均返回 Error（保留字符串 payload，其他 payload 使用固定消息），先清预留再释放 payload；B2 不发布孤立 entry，也不等待永不完成的初始 snapshot future。无子进程的失败不影响后续 list/get/output/wait/stop 或 dispose。payload 析构再 panic 仍可能传到调用任务，但预留已释放。

## 范围

仅 shell start 的首次 now 与同步 spawn 失败后的第二次 now；未改变正常 spawn、snapshot/输出协议。B2 与 TS 的差异是无孤立 failed entry、dispose 可完成。终态 shell、无 pid stop、subagent 完成/stop 的 Clock 仍待后续包，不扩大本条。真实 provider/UI 验收不在本包。

## 机器判据

`background.rs` 的 `clock_start_panic_releases_reservations_and_observers` 覆盖 B1/B2 × 重试/并发 dispose（含取消一名已注册观察者），`clock_start_payload_drop_happens_after_release` 验证析构顺序，`clock_start_failure_does_not_wait_for_snapshot` 验证永不完成前驱；`background_clock_regenerates_identically` 重生成金样。配对负控为 `runtime-spawn-panic/negative_controls.py --only clock-catch --only clock-locked --only clock-payload-order --only clock-pending-snapshot --only clock-drop --only clock-transfer --only clock-idempotent`，需提交后运行。

## 批准记录

未批准，非已接受差异。依据本轮用户 #804 指令实现并登记候选，不新增判分豁免、不修改冻结尺子；待协调者负控、两家非作者评审与人复核。
