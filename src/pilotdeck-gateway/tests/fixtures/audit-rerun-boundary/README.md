# audit-rerun-boundary 负控（#1723）

本目录的 `negative_controls.py` 验证 #1723 加在 `test_process.rs` 审计/重跑装置上的
九条边界守卫必须在被破坏时让绑定的父测试变红（各自在 `#1723` 消息断言处）：

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
- `ended-its-own-merged`（fix-r4）：合并判决谓词 `ended_its_own_or_unstopped`
  （`rerun_audit_case`、grandchild 用例、slack #2322 retention worker 三处共用的
  通过条件）丢掉 `finished_on_its_own` 臂、退回拆分前的时钟判决 → 审计的通过
  条件又把 deadline 轮询窗内自行跑完的 leader 读作 stopped；绑定该谓词自身的
  契约测试（与 `finished-on-its-own-merged` 同理：竞态窗窄到无法经 rerun_exact
  确定性驱动，停止路径的状态是 SIGKILL、永不是 `success()`，确定性路径抓不住
  丢掉的臂）。
- `probe-hang-deadline`（fix-r4）：探针命令换成无界的 `sleep 100` → 探针永不
  写盘，10 s 探针期限必须组杀它并让绑定父测试死在探针自身的 panic 处——这条
  上界正是防「卡住的探针经 OnceLock 钉死此后每个 rerun_exact 调用者」的那道；
  绑定 `owned_rerun_hits_the_kernel_output_cap`（确定性：sleep 远超期限，任何
  宿主上击杀都落在期限内）。反向变异（去掉期限本身）只会挂起父测试、按判据记
  INVALID 而非 KILLED，故刻意不立行。

每行先跑未变异的正控，再编译变异（不编译即 INVALID，不是 kill）、跑负控、恢复后
复跑正控；逐行记录 KILLED/SURVIVED/INVALID/SKIPPED 与原因，末尾汇总。绑定
`#[cfg(target_os = "linux")]` 父测试的行（`reuse-not-flagged`、
`stopped-not-recorded`、`children-only-sweeps-nothing`）在非 Linux 宿主上记
SKIPPED、不判 INVALID——但前提是**实证**的，不是白名单断言的：锚点计数与绑定父
测试自身的 `fn <leaf>(` 定义在包括 SKIPPED 行的一切宿主上都查（父测试改名/删除保
住锚点计数，否则会永远静默 SKIP），且记 SKIPPED 前必须在**树锁内**（`mutating_sources`
排他段，与正控行同规：listing 编译的是基线而不是并发 runner 的变异体）跑一次
`cargo test --list`：全表必须带一条已知必在的对照测试（探活，防空表把「不存在」
读成「cfg 掉」），且选不出该父测试；选得中（cfg 放宽）或源码里没有该 fn（改名）
即记 INVALID 并非零退出，绝不「静默 SKIP + exit 0」。
任一行非 KILLED 亦非 SKIPPED 即非零退出，零 KILLED 也非零。

运行（提交后，宿主；工作树守卫 `mutating_sources` 会拒绝脏树）：

```sh
CARGO_TARGET_DIR=<目标目录> \
  python3 src/pilotdeck-gateway/tests/fixtures/audit-rerun-boundary/negative_controls.py <新证据目录>
```

锚点过期（count != 1）记 INVALID 并继续，不得静默 SURVIVED；锚点串与绑定断言
都带本单专属 `#1723` 消息标记。
