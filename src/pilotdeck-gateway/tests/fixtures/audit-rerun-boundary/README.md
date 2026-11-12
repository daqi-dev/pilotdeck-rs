# audit-rerun-boundary 负控（#1723）

本目录的 `negative_controls.py` 验证 #1723 加在 `test_process.rs` 审计/重跑装置上的
七条边界守卫必须在被破坏时让绑定的父测试变红（各自在 `#1723` 消息断言处）：

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
- `finished-on-its-own-merged`：`finished_on_its_own` 退回拆分前的合并判决（时钟
  事实 `timed_out` 而非结局）→ 在 deadline 轮询窗内自行跑完的 leader 又被读作
  stopped；绑定谓词自身的契约测试（直接构造 `RerunOutput`：该竞态窗窄到无法经
  `rerun_exact` 确定性驱动，被删的字面改写在一切确定性路径上都是死代码，契约靠
  构造锁定）。
- `fsize-unit-constant-trusted`：单位探针把「用测得值」改回「信任编译期常数」
  （2048 B 在每个已实测宿主上都错）→ 漂移的单位表必须在任何 rerun 之前
  fail-closed；绑定 `owned_rerun_hits_the_kernel_output_cap`（探针先于一切
  rerun 运行，父测试死在探针自身断言处）。

每行先跑未变异的正控，再编译变异（不编译即 INVALID，不是 kill）、跑负控、恢复后
复跑正控；逐行记录 KILLED/SURVIVED/INVALID/SKIPPED 与原因，末尾汇总。绑定
`#[cfg(target_os = "linux")]` 父测试的行（`reuse-not-flagged`、
`stopped-not-recorded`、`children-only-sweeps-nothing`）在非 Linux 宿主上记
SKIPPED、不判 INVALID——但前提是**实证**的，不是白名单断言的：锚点计数在包括
SKIPPED 行的一切宿主上都查，且记 SKIPPED 前必须跑一次
`cargo test --list --exact <父测试>` 亲眼看到选不出（0 tests）；listing 选得中
（cfg 放宽、测试改名）即记 INVALID 并非零退出，绝不「静默 SKIP + exit 0」。
任一行非 KILLED 亦非 SKIPPED 即非零退出，零 KILLED 也非零。

运行（提交后，宿主；工作树守卫 `mutating_sources` 会拒绝脏树）：

```sh
CARGO_TARGET_DIR=<目标目录> \
  python3 src/pilotdeck-gateway/tests/fixtures/audit-rerun-boundary/negative_controls.py <新证据目录>
```

锚点过期（count != 1）记 INVALID 并继续，不得静默 SURVIVED；锚点串与绑定断言
都带本单专属 `#1723` 消息标记。
