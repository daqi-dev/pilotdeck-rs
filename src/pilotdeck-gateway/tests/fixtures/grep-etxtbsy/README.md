# #1125 grep-etxtbsy 负控

绑定测试：`runtime::tools::builtin::grep::tests::script_spawn_survives_concurrent_forks`
（grep.rs 单测，默认跑）。该测试经共享辅助
`tests/common/scratch.rs::write_executable`「临时名写入 + fsync + chmod + close +
rename」发布脚本后立即 exec，同时 4 个并发线程持续 fork——复现多线程 libtest
里 `failure_cleanup_kills_a_member_pid_without_signalling_its_group` 撞
ETXTBSY（`Os { code: 26 }`，cb8 Linux，见 `_tmp/20261026-issue-1123/
remote-host-cb8/host-lib.log`）的竞态条件。

变异体（均须在 Linux 上被压测以 `#1125 iteration` 断言杀死）：

- `helper-writes-final-path`：临时名指向最终路径（rename 变同名 no-op），
  直接写被 exec 的路径；
- `publish-by-copy`：`fs::rename` 改 `fs::copy`，目标 inode 被直接写入；
- `script-direct-write`：`Scratch::script` 退回修前 `fs::write` + chmod。

## 运行

提交候选后（被改源码须等于 HEAD），在 **Linux** 宿主上：

```sh
CARGO_TARGET_DIR=<绝对路径> python3 -I negative_controls.py NEW_OUT_DIR
# 锚点新鲜度预检（须打印 `3 anchors: ok` 后退出 0）
python3 -I negative_controls.py --check-anchors
```

macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，变异在该平台
会 SURVIVED——负控只应在 Linux 上运行与判读。cargo 不得带 `-q`（共享执行器
靠逐测试行判读）。
