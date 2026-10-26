---
id: A7-BACKGROUND-CLOCK-TAIL-001
---

## 类别

bug 修复：#804 B3/B4/S2/S3 的终态 Clock panic 会毒化 entry 锁、漏完成通知或取消信号，导致观察者挂起。

## 基线依据

cc7c4e6 `src/task/runtime/BackgroundTaskRuntime.ts:332-361` 的 exit resolve/reject 回调在更新状态前调用 now；真实探针 `src/pilotdeck-gateway/tests/fixtures/background-clock/generate.mts` 的 B-zero/B-nonzero/B-reject 均留下 running、无 endedAt、无 done/event、容量满、stop/dispose pending。B-never 验证无 pid 且无 exit 的既有挂起；TS 此路径没有新的终态 Clock 调用。原始输出 `_tmp/20261026-issue-804c/ts-probe.log`，金样 oracle.json 含基线与文件哈希。

## Rust 行为

`runtime/background.rs` 复用 catch_port 与 cleanup_task::lock：在原终态/取消线性化点捕获 Clock panic，保留原完成/失败/取消结果，ended_at 兜底为 started_at。先解锁并发布 done/完成事件或 cancellation，再释放 panic payload；payload 析构二次 panic 可以传到所属任务，但不会遗留 entry 毒锁、未发取消或未兑现 done。B3 保留真实 exit code，B4 保留无 pid 取消的既有语义。

## 范围

B3 的三条 TS 端口结果有真实探针；Rust 真实子进程测试覆盖 exit 0/7。TS promise rejection 不对应 Rust 原生 child.wait 的可注入错误分支，不冒充已跑 Rust wait 系统错误。B4 依已有 async-spawn 差异继续取消；subagent 基线无 Clock 端口，S2/S3 panic 属 Rust 专属，正常完成/失败/pending stop/running 合作取消已实测 TS。不处理 #794 run 构造 panic、永不返回的同步 Clock 或无视取消永不完成的 body；不改变它们的等待语义。

## 机器判据

clock_tail_shell_exit/shell_failure/no_pid/dispose/subagent_finish/subagent_failure/subagent_stop/subagent_pending_stop 独立用例；clock_tail_payload_drop 覆盖四条生命周期路径的析构顺序；原 background_clock_regenerates_identically 逐字节重生成扩充后的金样。配对变异见 runtime-spawn-panic/negative_controls.py 的 tail-*，脏树 guard 不可绕过，提交后由协调者宿主运行。

## 批准记录

未批准，非已接受差异。依据用户 #804 pr2 范围修复并登记候选；不修改冻结标准、不新增判分豁免，待协调者宿主负控、非作者评审及人复核。
