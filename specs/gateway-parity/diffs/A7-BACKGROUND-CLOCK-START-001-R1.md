---
id: A7-BACKGROUND-CLOCK-START-001-R1
original: A7-BACKGROUND-CLOCK-START-001
---

## 类别

bug 修复：补齐 B2 持久化收尾。原登记只保证没有内存孤立 entry，遗漏已接受的 Pending 快照会在重启后成为 Unknown。

## 基线依据

固定 cc7c4e6，`BackgroundTaskRuntime.ts:300-319` 先持久化 Pending，spawn 失败后修改内存 failed，再调用 now；该 Clock 抛错时没有终态写入。真实 JsonFile store 探针为 `tests/fixtures/background-clock/generate.mts` 的 durableCase；实测内存 failed/无 endedAt、磁盘 pending、重新构造 runtime 后 unknown。原始 Node 输出 `_tmp/20261026-issue-804c/fix-r1/ts-probe3.log`；来源哈希随金样登记。恢复构造会再向 store 写 Unknown，探针在恢复前取磁盘快照投影。

## Rust 行为

`runtime/background.rs` 同步 spawn 失败后，在再次调用 Clock 前接受 Failed 快照（没有 ended_at、没有输出）。正常 Clock 返回后仍走原终态持久化；Clock panic 返回 Error，仍无内存 entry，但磁盘保留已知 Failed，重启恢复 Failed。snapshot write 的返回观察 future 不阻塞 start，真实 store 自持有写链；payload 析构再 panic 也发生在 Failed 写接受之后。

## 范围

仅同步 spawn 失败的补偿快照；B1 尚未接受 Pending，无补偿需求。B2 的无内存 entry、Error 返回、dispose 可完成沿用原登记。与 TS 的新增差异是磁盘 Failed / 恢复 Failed，避免虚假 Unknown；无 ended_at 不捏造 Clock 时间。永久阻塞的 snapshot 返回 future 不改变既有有界错误返回：TS 的 pendingSnapshotCase 实测在初始写前驱停留（shell 调用 0、activeStarts 1、dispose 未结束），Rust 的 write 同步接受而观察 future 可丢弃，能继续 spawn 校验、补写 Failed 并释放预留。这是原实现已登记的调度差异，此处补全实测；真实 IO 错误仍按原 store 语义处理。

## 机器判据

`clock_start_failure_persists_terminal_snapshot` 用真实 JsonFile store 覆盖普通 Clock panic 与 payload 析构二次 panic、flush 后重新打开 store/runtime；`clock_start_failure_does_not_wait_for_snapshot` 覆盖永不完成观察 future，并消费 pendingSnapshotCase；clock_pending_snapshot_oracle_preserves_ts_observation 固定 TS 原始整行，负控 clock-pending-evidence。负控 `clock-failed-snapshot`；既有 `background_clock_regenerates_identically` 重新生成扩充的金样。负控提交后由协调者运行，详见本轮 RESULT。

## 批准记录

2026-10-26 #804 pr2 fix-r1 用户明确要求修复 B2 Pending 幽灵任务，允许补终态并登记差异。本更正执行该修复授权；不标记阶段或真人实效完成。
