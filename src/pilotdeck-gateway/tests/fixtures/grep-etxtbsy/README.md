# #1125 grep-etxtbsy 负控

绑定测试：`runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks`
（grep.rs 单测，默认跑）。该测试经共享辅助
`tests/common/scratch.rs::write_executable` 把脚本正文经管道喂给独立
`/bin/sh -c 'exec cat >"$tmp"'` 子进程写入后才 chmod + rename 发布，然后立即
exec，同时 4 个并发线程持续 fork——复现多线程 libtest 里
`failure_cleanup_kills_a_member_pid_without_signalling_its_group` 撞 ETXTBSY
（`Os { code: 26 }`，cb8 Linux，见 `_tmp/20261026-issue-1123/
remote-host-cb8/host-lib.log`）的竞态条件。写 fd 只存在于 helper 子进程，
本进程的任何 fork 都无法继承它，收割后没有任何进程再持有该文件写 fd，
故 exec 不可能 ETXTBSY（cb8 探针：进程内写最终路径形状 300 次压测撞 28 次
[fix-r1 探针 c，`_tmp/20261026-issue-1125/fix-r1/cb8-probe-c.log`]；helper
写入形状 0 次）。

fix-r3 起收尾结构：`ReapGuard` 从 helper spawn 起持有它，一切非成功退出
（错误返回、截止超时、panic 展开）都经 guard 的 Drop 单点 kill + wait 收割、
把 kill+wait 结果交给 `reaped` 钩子、删除临时名；写端设为非阻塞，writer
线程与父进程共享同一 expiry，读端被任何进程卡住也只放弃而不挂死
（fix-r3 评审 P2-1/P2-2）。

变异体：

- `script-direct-write`：`Scratch::script` 退回修前 `fs::write` + chmod（进程内
  直写被 exec 的路径）——须在 Linux 压测下以 `#1125 etxtbsy` 断言杀死；
- `helper-in-process-write`：`write_executable_bounded` 整个函数体退回 fix-r1
  变异形状（父进程 `File::create` 直写最终路径，写 fd 开到函数尾、exec 前；
  cb8 探针 c 实证 28/300）——同上。fix-r2 曾把该变异缩成「只写临时名、立即
  关 fd」，宿主实测 SURVIVED（窗口太窄，杀不死），fix-r3 按评审 P2 改回可杀
  形状；
- `no-temp-cleanup`：删掉 guard Drop 里的临时名清理，绑定
  `test_scratch_publication::failed_publication_removes_the_temporary_name`
  的 `#1125 temp residue` 断言（确定性，不依赖 Linux）；
- `deadline-kill-removed`：删掉 guard Drop 的 kill + wait + 报告，绑定
  `test_scratch_publication::a_stalled_helper_is_killed_and_cleaned_up`
  的 `#1125 stalled helper kill must be reported` 断言（确定性）；
- `deadline-error-kind`：超时错误 `TimedOut` 改成 `Other`（一处函数体覆盖
  writer 放弃、recv 兜底、wait 超时三个构造点），绑定
  `test_scratch_publication::a_helper_that_never_reads_hits_the_write_deadline`
  的 `#1125 deadline error must be TimedOut` 断言（确定性）；
- `writer-deadline-removed`：writer 的截止放弃检查改成 `if false`，绑定
  `test_scratch_publication::a_stranger_on_the_read_end_still_hits_the_write_deadline`
  的 `#1125 helper writer did not exit by its deadline` 断言（确定性）；
- `deadline-write-end-kept-open`：writer 的截止放弃分支泄漏一个写端克隆
  （`std::mem::forget(stdin.as_fd().try_clone_to_owned())`，
  `ChildStdin` 无 `try_clone`），本进程在放弃后仍持有写端，
  陌生进程读不到 EOF、marker 不出现——绑定
  `test_scratch_publication::a_dead_helper_with_a_stranger_reader_still_closes_the_write_end`
  的 `#1141 the write end must be closed` 断言（确定性；该测试是唯一观察
  写端关闭的用例，#1141）。

`test_scratch_publication` 是 lib 专属测试模块
（`src/test_scratch_publication.rs`，`#[cfg(all(test, unix))]`，fix-r3 评审 P2
与 `test_write_stage` 对齐）：`tests/common/scratch.rs` 被约 35 个集成测试
crate 以 `#[path]` 各编一份，共享文件里不再放单测，避免每份拷贝都跑一遍
（fix-r2 评审 P2）。

判杀只认 ETXTBSY：压测对 `raw_os_error() == Some(26)` 单独发
`#1125 etxtbsy` 标记，其他 exec 错误（如 EAGAIN）发 `#1125 exec-error`，
在 `classify` 里一律 INVALID，不得 KILLED。

## 运行

提交候选后（被改源码须等于 HEAD），在 **Linux** 宿主上：

```sh
CARGO_TARGET_DIR=<绝对路径> python3 -I negative_controls.py NEW_OUT_DIR
# 锚点新鲜度预检（须打印 `7 anchors: ok` 后退出 0）
python3 -I negative_controls.py --check-anchors
```

平台契约：macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，
`script-direct-write` / `helper-in-process-write` 在该平台预期 SURVIVED
（或因无关失败 INVALID）——这两个竞态负控是 **Linux 契约**，判杀只在
Linux 宿主（cb8 / CI）上成立；`no-temp-cleanup` 与四个 deadline/writer
变异体在任意 unix 宿主上确定性判杀（unix 平台无关，非 unix 无该测试模块）。
cargo 不得带 `-q`（共享执行器靠逐测试行判读）。
