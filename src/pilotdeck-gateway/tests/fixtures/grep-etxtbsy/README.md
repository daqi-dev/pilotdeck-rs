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
（fix-r3 评审 P2-1/P2-2）。fix-r2（#1141）起 `spawned` 钩子把 spawn 返回的
pid（对 `process_group(0)` 的 helper 即进程组号）在 guard 接管的同一刻交给
调用方，清理不再依赖子进程事后写组号文件；dead-stranger 用例的陌生进程是
一个 python3 探针，go 信号后用零超时 `select` + `os.read` 对读端分类（0
字节 = EOF 挂起 = 返回前全部写端已关；未就绪 = 仍有写端活过返回边界），
判定原子写进 marker。fix-r3（#1141，Linux cb8）起：helper 先 `exec 3<&0`
把读端存到 fd 3、后台命令再 `<&3`（dash 对后台命令即使显式 `<&0` 也指
`/dev/null`，读端会随 helper 灭、writer 得 EPIPE；bash 则不会——fd 3 保存
在两个 shell 下都保住读端）；探针的第一件事是 `fstat(0)` 确认拿到的是管道
（S_ISFIFO）才发布 `started`，否则立刻把 `stdin-not-a-pipe` 写进 marker；
helper `exit 0` 前与测试的 `spawned` 钩子都等 `started`，所以 helper 死亡、
writer 开写都在陌生进程确认持有读端之后（握手全部走信号文件 + 有界轮询）。

#1155 起的四项收紧：返回边界证明改为发布函数自身的 `/proc` 扫描——
`write_executable_bounded` 先 `fstat` 出管道 inode，`published` 算完后、guard
解除前经新 `returning(pipe)` 钩子扫描 `/proc/*/fd`，任何以 O_WRONLY/O_RDWR
打开该 `pipe:[inode]` 的 fd 都是泄漏，扫描错误优先于 `TimedOut` 合入返回值
（写端 fd 在本进程的最后一份数据 writer 线程，`is_finished` 已在返回前断言，
故健全性成立；候选持有者需在 500ms 瞬态宽限后仍在才算泄漏——本进程的
未 exec fork 在 fork→exec 窗口里继承 CLOEXEC 写端副本，这正是 #1125 的
窗口，瞬态不是泄漏，压测用例与这些用例并行 fork 时不得误报）；测试进程
成为 child subreaper（rustix 安全 API，进程级标志
以引用计数管理并行测试），孤儿陌生进程重挂到测试：`StrangerGuard` 在组 kill
后先用 NOWAIT `waitid` 确认陌生进程确已重挂为本进程子进程（Linux 上
`ECHILD` 直接报错——「由测试收割、不依赖 PID 1」成为被测不变量），再以
`waitpgid` 收割组内全部死者（含 helper started-poll 被 kill 打断时留下的
`/bin/sleep 0.05` 孤儿——subreaper 下它们也没有 init 兜底）；helper 侧
started 等待轮数由
`STRANGER_STARTED_TIMEOUT` 派生（单一来源），等到后原子写 `saw` 标记；
expiry 起点移到 `spawned(helper_id)` 之后，钩子等待不再吃写预算，测试以
钩子返回时刻为基准断言 `elapsed ∈ [deadline − 100ms, deadline + RETURN_GRACE)`。

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
  `ChildStdin` 无 `try_clone`），本进程在放弃后仍持有写端——#1155 r3 评审
  P2-1 起泄漏先被发布函数自身的返回边界扫描抓住（扫描错误优先于
  `TimedOut`），绑定 dead-helper 用例的
  `#1141 deadline error must be TimedOut` 断言（panic 载荷含扫描错误文本；
  go 信号后探针的 `write-end-still-held` 判定退居第二证据，确定性）；
- `parent-write-end-held-past-return`（#1141 fix-r2 评审 P2-1）：父进程在
  writer 起动前泄漏自己那份写端克隆（同一 `mem::forget` 形状，另一处持有
  者）——返回及时、自然死亡报告全部成立，泄漏克隆活过返回边界扫描，
  同上绑定 `#1141 deadline error must be TimedOut`（确定性；它取代 r1 的
  `deadline-write-end-released-late`——4s 定时释放只能靠 marker 时间戳
  分辨，已被探针判定取代）；
- `stranger-guard-kill-removed`（#1141 fix-r1 评审 P1-1）：删掉测试守卫
  `StrangerGuard` 的进程组 kill——停驻在 `sleep 30` 的陌生进程活过整个用例、
  进程组永不为空，绑定同一用例的 `#1141 the stranger must be killed and
  reaped by the test's guard` 断言（确定性；陌生进程在记完判定后停驻，
  只能由守卫回收）；
- `stranger-panic-guard-kill-removed`（#1141 fix-r2 评审 P1-1）：同一 kill
  删除，绑定新用例
  `test_scratch_publication::a_panic_while_the_stranger_runs_still_ends_it`
  ——发布函数在陌生进程已启动、后续步骤未跑时 panic（`spawned` 钩子里
  故意 panic，`catch_unwind` 捕获后显式收尾），守卫若不 kill，组不为空，
  `#1141 the stranger must still be killed and confirmed gone after a panic`
  失败；
- `spawn-hook-removed`（#1141 fix-r2 评审 P1-1）：`write_executable_bounded`
  的 `spawned(helper_id)` 交接被删——钩子不再被调用，窗口里没有 panic 发生、
  发布跑完整个截止错误，panic 用例的
  `#1141 the publication must panic while the stranger is running` 最先失败
  （其后紧跟的 `#1141 the guard must own the stranger's group from the spawn
  on` 所有权断言保证半途而废的钩子也不能空转通过）；
- `stranger-confirmation-inverted`（#1141 fix-r3，Linux cb8）：探针的 FIFO
  确认反转（`if not stat.S_ISFIFO(...)` → `if stat.S_ISFIFO(...)`）——陌生
  进程不再发布 `started`，dead-helper 用例 `spawned` 钩子里的 expect
  `#1141 the stranger must confirm it holds the pipe read end before the
  publication writes` 失败（任意 unix 宿主确定性判杀）；
- `stranger-retention-reverted`（#1141 fix-r3，Linux cb8）：helper 退回 r2
  形状（无 fd 3 保存、`<&0 &`、无 started 握手、无 saw 标记）。dash
  （Linux `/bin/sh`）对后台命令即使显式 `<&0` 也指 `/dev/null`，读端随
  helper 灭 → `started` 永不出现 → 同一 expect 失败；bash 显式 `<&0` 能保住
  读端，`started` 照常出现，但退回形状的 helper 不写 `saw` 标记，macOS 上
  由 #1155 r4 的 saw 断言判杀（任意 unix 宿主）；
- `expiry-hoisted-before-spawn`（#1155 r4 评审 P2）：expiry 移回 spawn 前
  ——`spawned` 钩子里的 6s `started` 握手吃光写预算，dead-helper 用例的
  `#1155 the write phase must last its full deadline` 下界断言失败（elapsed
  ≈ 0）；若 expiry 已过导致 writer-exit 竞态先行，则以
  `#1125 helper writer did not exit by its deadline` 判杀（双锚点，确定性）；
- `started-cap-hardcoded-short`（#1155 r4 评审 P2）：helper 侧轮数退回字面量
  100（5s）——6s 延迟的 `started` 出现前 helper 先放弃，死时无 `saw` 标记，
  `#1155 the helper must observe the stranger's started signal before it dies`
  判杀（确定性；发布本身照常失败返回，正是 r4 发现的「漂移对其他断言
  不可见」）；
- `boundary-write-end-filter-removed`（#1155 r3 评审 P2-1）：扫描去掉
  O_WRONLY/O_RDWR 访问模式过滤——健康路径上陌生进程的读端（同为
  `pipe:[inode]` fd）误报为存活写端，扫描错误进 `#1141 deadline error must
  be TimedOut` 的 panic 载荷（确定性）；
- `stranger-subreaper-removed`（#1155 r3 评审 P2-2，Linux）：subreaper 获取
  改 no-op——孤儿陌生进程落到 PID 1，守卫的 NOWAIT `waitid` 确认得
  `ECHILD`，严格 Linux 路径把它变成错误（`#1155 the stranger was not
  reparented to the test's guard`），守卫 end() 的 expect 判杀；
- `stranger-reap-removed`（#1155 r3 评审 P2-2，Linux）：`waitpgid` 组收割
  循环删除——subreaper 下没有 init 兜底，僵尸留在进程组里，组永不为空，
  `#1141 the stranger's process group survived the guard's kill` 超时报错，
  守卫 end() 的 expect 判杀。

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
# 锚点新鲜度预检（须打印 `18 anchors: ok` 后退出 0）
python3 -I negative_controls.py --check-anchors
```

平台契约：macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，
`script-direct-write` / `helper-in-process-write` 在该平台预期 SURVIVED
（或因无关失败 INVALID）——这两个竞态负控是 **Linux 契约**，判杀只在
Linux 宿主（cb8 / CI）上成立；`boundary-write-end-filter-removed` /
`stranger-subreaper-removed` / `stranger-reap-removed` 依赖 `/proc` 与
`PR_SET_CHILD_SUBREAPER`，同为 **Linux 契约**（非 Linux 内核上扫描与收割
被 cfg 掉，宿主预期 SURVIVED）；其余变异体在任意 unix 宿主上确定性判杀
（unix 平台无关，非 unix 无该测试模块）。
cargo 不得带 `-q`（共享执行器靠逐测试行判读）。
