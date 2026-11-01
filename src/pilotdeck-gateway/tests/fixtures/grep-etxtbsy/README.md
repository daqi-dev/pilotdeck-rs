# #1125 grep-etxtbsy 负控

绑定测试分两个 cargo target：

- `--lib`：`runtime::tools::builtin::grep::tests::script_spawn_survives_
  concurrent_forks`（grep.rs 单测，默认跑）。该测试经共享辅助
  `tests/common/scratch.rs::write_executable` 把脚本正文经管道喂给独立
  `/bin/sh -c 'exec cat >"$tmp"'` 子进程写入后才 chmod + rename 发布，然后立即
  exec，同时 4 个并发线程持续 fork——复现多线程 libtest 里
  `failure_cleanup_kills_a_member_pid_without_signalling_its_group` 撞 ETXTBSY
  （`Os { code: 26 }`，cb8 Linux）的竞态条件。写 fd 只存在于 helper 子进程，
  本进程的任何 fork 都无法继承它，收割后没有任何进程再持有该文件写 fd，
  故 exec 不可能 ETXTBSY（cb8 探针：进程内写最终路径形状 300 次压测撞 28 次
  [fix-r1 探针 c]；helper 写入形状 0 次）。
- `--test scratch_publication`：`tests/scratch_publication.rs`，#1155 fix-r1
  起的**专用集成测试二进制**（自有进程）。原先这些用例是 lib 内模块
  `src/test_scratch_publication.rs`；但 `StrangerGuard` 的 child-subreaper
  租约是进程级标志，libtest 线程池并行时其他测试留下的孤儿全部重挂到本
  进程而无人收割，僵尸钉住 `group_exists`，密集并行下 3/3 轮各杀死 5/7 个
  grep 组用例（60s watchdog 超时，`fix-r1/probe/dense-*.log`，#1155 fix-r1
  评审 P1）——移入专用二进制后租约只能收养本二进制内这些用例自己的孤儿，
  每个守卫只收割自己那组（`waitpgid` 组收割；无组的 helper 走 Linux 专属
  pid 模式，经其发布的 pid 判杀并收割，kill 错误不再丢弃而是传播）。

fix-r3 起收尾结构：`ReapGuard` 从 helper spawn 起持有它，一切非成功退出
（错误返回、截止超时、panic 展开）都经 guard 的 Drop 单点 kill + wait 收割、
把 kill+wait 结果交给 `reaped` 钩子、删除临时名；写端设为非阻塞，writer
线程与父进程共享同一 expiry，读端被任何进程卡住也只放弃而不挂死。fix-r2
（#1141）起 `spawned` 钩子把 spawn 返回的 pid（对 `process_group(0)` 的
helper 即进程组号）在 guard 接管的同一刻交给调用方；陌生进程是 python3
探针，`fstat(0)` 确认拿到管道才发布 `started`，helper 先 `exec 3<&0` 把读端
存到 fd 3（dash 对后台命令即使显式 `<&0` 也指 `/dev/null`）。

#1155 起的四项收紧：返回边界证明改为发布函数自身的 `/proc` 扫描——
`write_executable_bounded` 先 `fstat` 出管道 inode，`published` 算完后、guard
解除前经 `returning(pipe)` 钩子扫描 `/proc/*/fd`，任何以 O_WRONLY/O_RDWR
打开该 `pipe:[inode]` 的 fd 都是泄漏，扫描错误优先于 `TimedOut` 合入返回值；
**fdinfo 未知态 fail-closed**（fix-r1 评审 opus row 1）——fdinfo 读失败仅
ENOENT（fd 中途消失）允许 continue，其余错误、以及 flags 行缺失/不可解析，
都按扫描错误返回而不是当作读端放过。测试进程成为 child subreaper（进程级
标志以引用计数管理并行测试），孤儿陌生进程重挂到测试：守卫在组 kill 后先
用 NOWAIT `waitid` 确认陌生进程确已重挂（Linux 上 `ECHILD` 直接报错），再以
`waitpgid` 收割组内全部死者；expiry 起点移到 `spawned(helper_id)` 之后，
钩子等待不吃写预算。fix-r1 再收紧：`STRANGER_STARTED_DELAY` 6s→8s，helper
的 started 等待由轮数改为墙钟（`date +%s`，负载拉不长），测试侧 10s 上限
单一来源派生。fix-r2b（#1155 r2 评审）：helper 的截止秒数 `+ 1` 补偿
`date +%s` 整秒截断（仍单一来源派生，真实预算不低于测试侧上限）；
`end` 在启动 stranger 之前算出（计时起点统一）；**saw 写入前复检同一截止**
（Codex P1-1——轮询跨截止点暂停后恢复、文件已存在也不能把迟到 started 记
成 seen，新增受控用例 `a_started_past_the_helper_cap_is_not_seen_after_a_
paused_poll`）；fdinfo
读失败不再按 errno 分类放过——**复检 fd 本身的 readlink**，fd 仍在且仍指向
管道则 fail-closed，fd 已消失（任何 errno：ENOENT/ESRCH/EACCES，本机 6.17
实测：僵尸 fdinfo→ENOENT 而 fd 链接→EACCES、setuid exec 后→EACCES）即释放
（`fdinfo_unreadable`，配 Linux 单元测试）；pid 模式 stranger 由 `sleep 5`
改为停驻 `sleep 30`（与组模式一致，只有守卫 kill 能终结），kill 契约因此可
判杀。fix-r3（fix-r3 评审 P1-1）：受控用例的暂停改为**事件屏障**——helper
在算出 `end`、拉起 stranger 后原子发布 `paused` 收据，随后挂起等 `resume`
信号（0.05s 轮询、30s 安全上限，独立于 `end`）；用例等收据 → 等 `started`
→ 以「`end` ≤ 收据观测时刻 + cap」证同一截止已越过 → 才发布 `resume`，
saw 写入时 started 必在、截止必过，不再有任何睡眠时序赌注。共享 helper 改形
后 dead-helper / panic 两用例同步屏障化（钩子在 started 后发布 resume；panic
用例故意不发，由 ReapGuard 收割挂起的 helper）。

变异体（23 个；绑定测试如无说明均在 `--test scratch_publication`）：

- `script-direct-write`（`--lib`）：`Scratch::script` 退回修前 `fs::write` +
  chmod（进程内直写被 exec 的路径）——须在 Linux 压测下以 `#1125 etxtbsy`
  断言杀死；
- `helper-in-process-write`（`--lib`）：`write_executable_bounded` 整个函数体
  退回 fix-r1 变异形状（父进程 `File::create` 直写最终路径，写 fd 开到函数
  尾、exec 前；cb8 探针 c 实证 28/300）——同上；
- `no-temp-cleanup`：删掉 guard Drop 里的临时名清理，绑定
  `failed_publication_removes_the_temporary_name` 的 `#1125 temp residue`
  断言（确定性，不依赖 Linux）；
- `deadline-kill-removed`：删掉 guard Drop 的 kill + wait + 报告，绑定
  `a_stalled_helper_is_killed_and_cleaned_up` 的 `#1125 stalled helper kill
  must be reported` 断言（确定性）；
- `deadline-error-kind`：超时错误 `TimedOut` 改成 `Other`，绑定
  `a_helper_that_never_reads_hits_the_write_deadline` 的 `#1125 deadline
  error must be TimedOut` 断言（确定性）；
- `writer-deadline-removed`（fix-r1 改绑）：writer 的截止放弃检查改成
  `if false`，改绑 dead-helper 用例 `a_dead_helper_with_a_stranger_reader_
  still_closes_the_write_end` 的 `#1125 helper writer did not exit by its
  deadline` 断言（原绑 pid 模式用例，后者现为 Linux 契约；dead-helper 组用例
  在任意 unix 都能确定性判杀同一断言）；
- `deadline-write-end-kept-open`：writer 的截止放弃分支泄漏一个写端克隆
  （`mem::forget(stdin.as_fd().try_clone_to_owned())`），绑定 dead-helper
  用例的**双锚点列表** `#1141 deadline error must be TimedOut` / `#1141 the
  write end must be closed by the time the publication returns`（fix-r1 评审
  DeepSeek P1-1）：Linux 上泄漏先被发布函数自身的返回边界扫描抓住（扫描错误
  优先于 `TimedOut`，panic 载荷含扫描错误文本）；无 `/proc` 的宿主上发布照常
  返回 `TimedOut`，失败点退到 go 信号后探针的 `write-end-still-held` 判定。
  两平台均确定性；
- `parent-write-end-held-past-return`（#1141 fix-r2 评审 P2-1）：父进程在
  writer 起动前泄漏自己那份写端克隆（同一 `mem::forget` 形状）——同上双锚点
  绑定（确定性）；
- `stranger-guard-kill-removed`（#1141 fix-r1 评审 P1-1）：删掉测试守卫
  `StrangerGuard` 的进程组 kill——停驻的陌生进程活过整个用例，重挂确认报
  「未按 kill 死亡」，绑定同一用例的 `#1141 the stranger must be killed and
  reaped by the test's guard` 断言（确定性）；
- `stranger-panic-guard-kill-removed`：同一 kill 删除，绑定
  `a_panic_while_the_stranger_runs_still_ends_it` 的 `#1141 the stranger must
  still be killed and confirmed gone after a panic` 断言（确定性）；
- `spawn-hook-removed`：`write_executable_bounded` 的 `spawned(helper_id)`
  交接被删——panic 用例的 `#1141 the publication must panic while the
  stranger is running` 最先失败（其后紧跟的守卫所有权断言保证半途而废的钩子
  也不能空转通过）；
- `stranger-confirmation-inverted`：探针的 FIFO 确认反转——`started` 永不
  出现，dead-helper 用例 `spawned` 钩子里的 expect `#1141 the stranger must
  confirm it holds the pipe read end before the publication writes` 失败
  （任意 unix 宿主确定性判杀）；
- `stranger-retention-reverted`（fix-r1 双锚点，评审 opus row 3）：helper
  退回 r2 形状（无 fd 3 保存、`<&0 &`、无 started 握手、无 saw 标记）。dash
  （Linux `/bin/sh`）对后台命令即使显式 `<&0` 也指 `/dev/null`，读端随
  helper 灭 → `started` 永不出现 → 同一 expect 失败；bash 显式 `<&0` 能保住
  读端，`started` 照常出现，但退回形状的 helper 不写 `saw` 标记，由 saw
  断言 `#1155 the helper must observe the stranger's started signal before it
  dies` 判杀——两个失败点收进一个锚点列表（列表即备选集）；
- `expiry-hoisted-before-spawn`（#1155 r4 评审 P2）：expiry 移回 spawn 前
  ——`spawned` 钩子里的 8s `started` 握手吃光写预算，dead-helper 用例的
  `#1155 the write phase must last its full deadline` 下界断言失败（elapsed
  ≈ 0）；若 expiry 已过导致 writer-exit 竞态先行，则以 `#1125 helper writer
  did not exit by its deadline` 判杀（双锚点，确定性）；
- `started-cap-hardcoded-short`（#1155 r4 评审 P2，fix-r1 改形状，fix-r2b
  锚点带 `+ 1` 截断补偿，fix-r3 改绑）：helper 侧 started 等待秒数退回字面量
  `5`（墙钟，`date +%s` 基准）——fix-r3 起绑定走事件屏障受控路径的新用例
  `a_started_within_the_helper_cap_is_still_seen_after_a_paused_poll`：该用例
  在 8s 延迟的 `started` 出现的一刻恢复 helper，推导的 11s cap 当刻仍有预算、
  健康 helper 记录 `saw`；硬回 5s 的 cap 在同一受控时刻截止已过、拒写 saw，
  `#1155 a started within the helper's cap must still be recorded as seen
  after a paused poll` 判杀（确定性；不再依赖旧 dead-helper 用例的时序旁证）；
- `saw-write-skips-the-cap`（fix-r2b 新增，Codex P1-1；fix-r3 改屏障）：saw
  写入的截止复检被删——事件屏障用例等 `paused` 收据、2s 延迟的 `started`、
  并证 1s 上限的截止已越过后才恢复 helper，saw 写入时文件必在、截止必过，
  迟到 started 被记成 seen，`#1155 a started past the helper's cap
  must not be recorded as seen after a paused poll` 判杀（任意 unix 确定性，
  无调度时序赌注）；
- `boundary-write-end-filter-removed`（#1155 r3 评审 P2-1；fix-r1 改绑，
  DeepSeek P2-2）：扫描去掉 O_WRONLY/O_RDWR 访问模式过滤——健康路径上陌生
  进程的读端（同为 `pipe:[inode]` fd）误报为存活写端，扫描错误进 dead-helper
  用例 `#1141 deadline error must be TimedOut` 的 panic 载荷，绑定改认扫描
  自身载荷 `#1155 a write end of the publication pipe`（不再依赖扫描错误
  下游挤掉哪个断言）；
- `boundary-fdinfo-read-error`（fix-r1 新增，评审 opus row 1）：fdinfo 路径
  尾加斜杠 → 确定性 ENOTDIR → 走通 fail-closed 读分支，绑定扫描自身载荷
  `#1155 reading the fdinfo`（Linux 契约）；
- `boundary-fdinfo-parse-error`（fix-r1 新增，同上）：`strip_prefix`
  破坏 → flags 行永不可解析 → 走通 fail-closed 解析分支，绑定 `has no
  parsable flags line`（Linux 契约）；
- `boundary-fdinfo-unknown-passed`（fix-r2b 新增，boundary P2 / concurrency
  P3）：`fdinfo_unreadable` 的 fd 复检改成永不确定——单元测试的活 fd 半边
  在 expect 处失败，`#1155 an unreadable fdinfo behind a live fd must fail
  the scan closed` 判杀；其消失 fd 半边钉住释放侧（Linux 契约：/proc）；
- `stranger-subreaper-removed`（#1155 r3 评审 P2-2，Linux）：subreaper 获取
  改 no-op——孤儿陌生进程落到 PID 1，守卫的 NOWAIT `waitid` 确认得 `ECHILD`，
  严格 Linux 路径把它变成错误（`#1155 the stranger was not reparented to
  the test's guard`），守卫 end() 的 expect 判杀；
- `stranger-reap-removed`（#1155 r3 评审 P2-2，Linux）：`waitpgid` 组收割
  循环删除——租约下没有 init 兜底，僵尸留在进程组里，组永不为空，
  `#1141 the stranger's process group survived the guard's kill` 超时报错，
  守卫 end() 的 expect 判杀；
- `stranger-pid-kill-removed`（fix-r2b 新增，boundary P3，Linux；fix-r3 修
  绑定名）：pid 模式守卫的 kill 被删——stranger 停驻 `sleep 30` 不再自行退
  出，重挂确认 5s 超时报 `#1155 the stranger did not die by the guard's
  kill`，pid 模式用例 end() 的 expect `#1155 the stranger must be killed and
  reaped by the test's guard, not PID 1's` 判杀。**绑定名须带模块前缀**
  `pid_mode::…`（fix-r3 评审 P1-2：`--exact` 下裸名匹配 0 个测试，正控被
  `passed()` 拒绝、整行 INVALID）。kill 错误传播（fix-r1 P2-1 收紧的另一半）为
  无变异体的防御层：租约下僵尸钉住 pid，真实 kill 不会 ESRCH，本二进制
  可达路径上无失败可绑定（边界记录于 cases 内联注释与 fix-r2b RESULT）。

`tests/scratch_publication.rs` 是专用集成测试二进制：`tests/common/
scratch.rs` 被约 35 个集成测试 crate 以 `#[path]` 各编一份，共享文件里不
放单测，避免每份拷贝都跑一遍（fix-r2 评审 P2）；pid 模式（helper 无自有
进程组的用例及其专属件）收在 `#[cfg(target_os = "linux")] mod pid_mode`，
非 Linux 显式 no-op——pid 从 helper 发布的文件读回并 SIGKILL 只在 subreaper
租约下安全（未收割僵尸不可复用其 pid），fix-r1 评审 P2-1。

判杀只认 ETXTBSY：压测对 `raw_os_error() == Some(26)` 单独发 `#1125
etxtbsy` 标记，其他 exec 错误（如 EAGAIN）发 `#1125 exec-error`，在
`classify` 里一律 INVALID，不得 KILLED。

## 运行

提交候选后（被改源码须等于 HEAD），在 **Linux** 宿主上：

```sh
CARGO_TARGET_DIR=<绝对路径> python3 -I negative_controls.py NEW_OUT_DIR
# 锚点新鲜度预检（须打印 `23 anchors: ok` 后退出 0）
python3 -I negative_controls.py --check-anchors
```

每个变异体自带 cargo target：两个 ETXTBSY 竞态变异体跑 `--lib`（绑定
grep 压测），其余 21 个跑 `--test scratch_publication`。

平台契约：macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，
`script-direct-write` / `helper-in-process-write` 在该平台预期 SURVIVED
（或因无关失败 INVALID）——这两个竞态负控是 **Linux 契约**，判杀只在
Linux 宿主（cb8 / CI）上成立；`boundary-write-end-filter-removed` /
`boundary-fdinfo-read-error` / `boundary-fdinfo-parse-error` /
`boundary-fdinfo-unknown-passed` / `stranger-subreaper-removed` /
`stranger-reap-removed` / `stranger-pid-kill-removed` 依赖 `/proc` 与
`PR_SET_CHILD_SUBREAPER`（后两个中前者绑定 cfg linux 的单元测试、后者绑定
pid_mode 专属件），同为 **Linux 契约**（非 Linux 内核上扫描与收割
被 cfg 掉，宿主预期 SURVIVED）；其余 14 个变异体在任意 unix 宿主上确定性
判杀（unix 平台无关，非 unix 无该测试模块）。
cargo 不得带 `-q`（共享执行器靠逐测试行判读）。
