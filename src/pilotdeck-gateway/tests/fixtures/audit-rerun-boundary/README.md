# audit-rerun-boundary 负控（#1723）

本目录的 `negative_controls.py` 验证 #1723 加在 `test_process.rs` 审计/重跑装置上的
五条边界守卫必须在被破坏时让绑定的父测试变红（各自在 `#1723` 消息断言处）：

- `kernel-cap-removed`：wrapper 去掉 `ulimit -f` → 内核顶消失，捕获文件能在一个
  轮询间隔的写入里越过上限；绑定 `owned_rerun_hits_the_kernel_output_cap` 的
  字节上限断言。
- `hit-cap-not-overflow`：`>=` 判据退回采样时代的严格 `>` → 恰好停在顶上的文件
  被读作未触顶；绑定 `owned_rerun_stops_a_writer_that_survives_the_kernel_cap`
  的 overflow 断言（该写者确定性把文件停在恰好上限而 leader 存活）。
- `reuse-not-flagged`：`reappeared` 永不置位 → 离开过 sweep 又同 exe 归来的 pid
  读作 before-set 早就认识的子进程；绑定注入 proc root 的复用装置用例
  `the_sampler_reports_a_pid_that_leaves_and_returns` 的 transient 断言。
- `stopped-not-recorded`：停止分支不再记录 `stopped`；绑定
  `owned_rerun_kills_the_whole_group_at_the_deadline` 的 stopped 断言。
- `children-only-sweeps-nothing`：子进程 sweep 观测为空 → 窗口以空前集开窗、
  判决空转（正是该装置要抓的假绿）；绑定同一复用装置用例的 before-set 断言。

每行先跑未变异的正控，再编译变异（不编译即 INVALID，不是 kill）、跑负控、恢复后
复跑正控；逐行记录 KILLED/SURVIVED/INVALID 与原因，末尾汇总，任一行非 KILLED
即非零退出，零 KILLED 也非零。

运行（提交后，宿主；工作树守卫 `mutating_sources` 会拒绝脏树）：

```sh
CARGO_TARGET_DIR=<目标目录> \
  python3 src/pilotdeck-gateway/tests/fixtures/audit-rerun-boundary/negative_controls.py <新证据目录>
```

锚点过期（count != 1）记 INVALID 并继续，不得静默 SURVIVED；锚点串与绑定断言
都带本单专属 `#1723` 消息标记。
