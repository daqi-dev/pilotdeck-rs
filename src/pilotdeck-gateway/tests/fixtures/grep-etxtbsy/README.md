# #1125 grep-etxtbsy 负控

绑定测试：`runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks`
（grep.rs 单测，默认跑）。该测试经共享辅助
`tests/common/scratch.rs::write_executable` 把脚本正文经管道喂给独立
`/bin/sh -c 'cat >"$tmp"'` 子进程写入、在截止时间内收割（超时则 kill +
wait 收割 helper、删除临时名、返回 `TimedOut` 错误，fix-r2）后才 chmod +
rename 发布，然后立即 exec，同时 4 个并发线程持续 fork——复现多线程
libtest 里 `failure_cleanup_kills_a_member_pid_without_signalling_its_group`
撞 ETXTBSY（`Os { code: 26 }`，cb8 Linux，见 `_tmp/20261026-issue-1123/
remote-host-cb8/host-lib.log`）的竞态条件。写 fd 只存在于 helper 子进程，
本进程的任何 fork 都无法继承它，收割后没有任何进程再持有该文件写 fd，
故 exec 不可能 ETXTBSY（cb8 探针：进程内临时名写入+rename 形状 300 次压测
撞 28 次，helper 写入形状 0 次；fix-r1 的 rename 方案经评审 P1 判定不消除
继承 fd 竞态）。

变异体：

- `script-direct-write`：`Scratch::script` 退回修前 `fs::write` + chmod（进程内
  直写被 exec 的路径）——须在 Linux 压测下以 `#1125 etxtbsy` 断言杀死；
- `helper-in-process-write`：`write_executable` 的 helper spawn 段退回进程内
  直写临时名（fix-r1 形状；锚点只覆盖该 spawn 段，不复制整个函数体）——同上；
- `no-temp-cleanup`：删掉失败路径的临时名清理，绑定
  `test_scratch_publication::failed_publication_removes_the_temporary_name`
  的 `#1125 temp residue` 断言（确定性，不依赖 Linux）；
- `deadline-kill-removed`：删掉超时路径的 kill + wait 收割，绑定
  `test_scratch_publication::a_stalled_helper_is_killed_and_cleaned_up`
  的 `#1125 stalled helper must be killed and reaped` 断言（确定性）；
- `deadline-error-kind`：超时错误 `TimedOut` 改成 `Other`，绑定
  `test_scratch_publication::a_helper_that_never_reads_hits_the_write_deadline`
  的 `#1125 deadline error must be TimedOut` 断言（确定性）。

`test_scratch_publication` 是 lib 专属测试模块（`src/test_scratch_publication.rs`）：
`tests/common/scratch.rs` 被约 35 个集成测试 crate 以 `#[path]` 各编一份，
共享文件里不再放单测，避免每份拷贝都跑一遍（fix-r2 评审 P2）。

判杀只认 ETXTBSY：压测对 `raw_os_error() == Some(26)` 单独发
`#1125 etxtbsy` 标记，其他 exec 错误（如 EAGAIN）发 `#1125 exec-error`，
在 `classify` 里一律 INVALID，不得 KILLED。

## 运行

提交候选后（被改源码须等于 HEAD），在 **Linux** 宿主上：

```sh
CARGO_TARGET_DIR=<绝对路径> python3 -I negative_controls.py NEW_OUT_DIR
# 锚点新鲜度预检（须打印 `5 anchors: ok` 后退出 0）
python3 -I negative_controls.py --check-anchors
```

macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，
`script-direct-write` / `helper-in-process-write` 在该平台会 SURVIVED——
这两个负控只应在 Linux 上运行与判读（`no-temp-cleanup` 与两个 deadline
变异体平台无关）。
cargo 不得带 `-q`（共享执行器靠逐测试行判读）。
