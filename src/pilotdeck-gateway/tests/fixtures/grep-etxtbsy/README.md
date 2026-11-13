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
的 started 等待由轮数改为墙钟（`date +%s`，负载拉不长），测试侧上限
单一来源派生（当时 10s，#1297 二轮评审后放宽到 14s，见下段）。fix-r2b
（#1155 r2 评审）：helper 的截止秒数 `+ 1` 补偿
`date +%s` 整秒截断（仍单一来源派生，真实预算不低于测试侧上限）；
`end` 在启动 stranger 之前算出（计时起点统一）；**saw 写入前复检同一截止**
（Codex P1-1——轮询跨截止点暂停后恢复、文件已存在也不能把迟到 started 记
成 seen，新增受控用例 `a_started_past_the_helper_cap_is_not_seen_after_a_
paused_poll`）；fdinfo
读失败不再按 errno 分类放过——**复检 fd 本身的 readlink**，fd 仍在且仍指向
管道则 fail-closed（`fdinfo_unreadable`，配 Linux 单元测试）。**fix-r2b 当时
的口径是「fd 已消失（任何 errno：ENOENT/ESRCH/EACCES）即释放」，#1409 起这
半句已作废**：本机 6.17 实测的僵尸 fdinfo→ENOENT 而 fd 链接→EACCES、setuid
exec 后→EACCES 两个事实仍然成立，但 EACCES 不再自动等于释放（僵尸靠「整组
已终态」才释放，setuid exec 后的活持有者必须算仍持有）。#1409 起两级读分成：
`read_dir` 失败交给来源分类（自身/后代/关系读不出→关；已消失→跳过；
`Z`/`X` 只有在整个线程组已无活任务时才算终态——组长 `pthread_exit()` 后
组仍持 fd 表，见 r2 的两个变异体），
`read_link` 只有 ENOENT 算释放、其余 errno 一律由来源裁决（持有者已消失或
已终态才算释放，本进程/后代/读不出状态的一律当作仍持有）——此前两级都是
「读不到就 continue」，一条都看不见也照样出干净结论。这条规则只有一处实现
`link_failure_is_a_release`，三处 `read_link`（枚举、fdinfo 复检、宽限复检）
都走它，所以三处不会各走各的漂移。
pid 模式 stranger 由 `sleep 5`
改为停驻 `sleep 30`（与组模式一致，只有守卫 kill 能终结），kill 契约因此可
判杀。fix-r3（fix-r3 评审 P1-1）：受控用例的暂停改为**事件屏障**——helper
在算出 `end`、拉起 stranger 后原子发布 `paused` 收据，随后挂起等 `resume`
信号（0.05s 轮询、30s 安全上限，独立于 `end`）；用例等收据 → 等 `started`
→ 以「`end` ≤ 收据观测时刻 + cap」证同一截止已越过 → 才发布 `resume`，
saw 写入时 started 必在、截止必过，不再有任何睡眠时序赌注。共享 helper 改形
后 dead-helper / panic 两用例同步屏障化（钩子在 started 后发布 resume；panic
用例故意不发，由 ReapGuard 收割挂起的 helper）。#1392（#1155 r4 评审 P1 →
P2b 跟进）：屏障自身的 30s 安全超时是失败而非静默通过——循环后强制复查
`resume`（循环条件与退出之间的竞态由复查兜住），未收到即 `exit 3`；saw 写入
仍先行执行，超时路径继续走截止复检，删复检的变异在该路径照样可杀；新增
「屏障超时」交错用例（测试侧故意不发 `resume`，断言 helper 非零退出、退出码
3、park 实跑 ~30s、saw 不写）。#1297（批次 B10）：dead-helper 用例的
`started` 握手不再赌固定睡眠——探针写好 staging 后等待 `started + ".release"`
信号文件（50ms 轮询、60s 上限），测试钩子在 adopt 后立刻原子发布
（staging+rename）；原先钩子要陪跑探针里固定的 8s 睡眠，10s `started` 上限在
重载下（4 份并发 lib 副本为负载）会先耗尽。release 门控健康路径（测试自己拥有
该文件，在探针 boot 之前就发出），但**不是硬前置**：探针那条等待另有 60s 兜底，
超时后照样发布 `started`（三轮评审 DeepSeek concurrency P3 + MiniMax boundary P3
指出文档与实现相反）——该兜底今天不可达，因为测试侧 14s 的 `started` 上限先失败。
所以实际时刻由「解释器启动（本机实测 ~15ms）+ 用例自己的节拍睡眠」决定——
节拍不是装饰，见下段。
deadline 下界余量 `DEADLINE_FLOOR_SLACK` 随之
100ms→5ms——健康路径本就用不到余量（expiry 在 `write_began` 之后才计算，
写阶段只能超时不能提前），而该余量同时是 `expiry-hoisted-before-spawn` 的
判杀线。#1297 评审（concurrency P1 + 两家二轮的 sibling/boundary/concurrency）：
判杀裕度是 `hook 耗时 − 余量 − 返回路径耗时`，事件化后 hook 的保证内容只剩
解释器启动，负载可能把它饿到 5ms 以下；返回路径（扫描、kill、收割）也在被测
的 `took` 里，会进一步吃掉裕度，Linux 上这条路径还含 500ms 宽限的
返回边界 `/proc` 扫描。故 dead-helper 用例的探针改为在确认持有管道后**故意
停 `DEAD_HELPER_STRANGER_DELAY`（1s）**再发 `started`：这是测试自己选的
节拍，不是等 cap，距 14s `started` 上限仍有 14× 余量，判杀裕度回到 ~1s，
且高于 Linux 扫描的 500ms 宽限；删掉或缩短它会连带拿走该变异体的判杀裕度。
panic 用例
与三个场景用例（迟到 started / within-cap / 屏障超时）同样先发布 release
再等信号；within-cap 仍以 8s 延迟的 `started` 暴露硬回 5s 的 cap，因此
`STRANGER_STARTED_TIMEOUT` 10s→14s（旧值只给探针启动留 2s，正是 #1297 要
修的那种「固定延迟贴着上限」形状；8s 延迟本身不能缩短——它要明显越过
硬回的 5s cap，见变异行）。

变异体（条数与平台契约见文件末尾的 `mutants` / `linux-contract` 标记：两者
由 `--check-anchors` 从 `cases` 与 `SURVIVES_OFF_LINUX` / `INVALID_OFF_LINUX`
派生并逐项比对，手写数字不再进正文；绑定测试如无说明均在
`--test scratch_publication`）：

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
  ——`spawned` 钩子里等待 `started` 的整段耗时（#1297 起是探针解释器启动 +
  `DEAD_HELPER_STRANGER_DELAY` 的 1s 节拍）落进写预算，dead-helper 用例的
  `#1155 the write phase must last its full deadline` 下界断言失败（本机 darwin
  实测 `458.24875ms elapsed`，比 `1.5s − 5ms` 的阈值低 ~1.04s，即判杀裕度
  ≈ 1s，不再依赖解释器启动那 15ms，也高于 Linux 返回路径 `/proc` 扫描的
  500ms 宽限；原始输出 `_tmp/20261106-1495-s2/negctl-r2.log`）；若 expiry 已过
  导致 writer-exit 竞态先行，则以
  `#1125 helper writer did not exit by its deadline` 判杀（该锚点只在极端饥饿
  下先触发，正常挂钟下由 floor 断言判杀——确定性来自墙钟算术，不来自两条断言
  都会命中）；
- `started-cap-hardcoded-short`（#1155 r4 评审 P2，fix-r1 改形状，fix-r2b
  锚点带 `+ 1` 截断补偿，fix-r3 改绑）：helper 侧 started 等待秒数退回字面量
  `5`（墙钟，`date +%s` 基准）——fix-r3 起绑定走事件屏障受控路径的新用例
  `a_started_within_the_helper_cap_is_still_seen_after_a_paused_poll`：该用例
  在 8s 延迟的 `started` 出现的一刻恢复 helper，推导的 15s cap 当刻仍有预算、
  健康 helper 记录 `saw`；硬回 5s 的 cap 在同一受控时刻截止已过、拒写 saw，
  `#1155 a started within the helper's cap must still be recorded as seen
  after a paused poll` 判杀（确定性；不再依赖旧 dead-helper 用例的时序旁证）；
- `saw-write-skips-the-cap`（fix-r2b 新增，Codex P1-1；fix-r3 改屏障）：saw
  写入的截止复检被删——事件屏障用例等 `paused` 收据、2s 延迟的 `started`、
  并证 1s 上限的截止已越过后才恢复 helper，saw 写入时文件必在、截止必过，
  迟到 started 被记成 seen，`#1155 a started past the helper's cap
  must not be recorded as seen after a paused poll` 判杀（任意 unix 确定性，
  无调度时序赌注）；
- `barrier-timeout-exit-removed`（#1392 新增，#1155 r4 评审 P1 → P2b 跟进）：
  屏障循环后的强制 resume 复查被删——30s 安全超时不再 `exit 3`，helper 照常
  干净退出；新用例 `a_barrier_timeout_without_a_resume_fails_the_helper`
  故意不发 `resume` 让 park 自行跑完，`#1392 a barrier timed out without a
  resume must fail the helper, not exit cleanly` 判杀（r4 评审的双双饥饿 > 30s
  交错由此堵死：测试线程与 stranger 同时被饿过 park 时，helper 不可能再以
  `exit 0` 溜过 `exited.success()` 断言；任意 unix 确定性）；
- `saw-write-skips-the-cap-barrier-timeout`（#1392 新增）：同一「截止复检被删」
  变异改绑屏障超时用例（与守卫 kill 删除的两个绑定同形，一变异两绑定）——该
  用例等 `paused`、2s 延迟的 `started` 后永不发 `resume`，超时时刻 started 必
  在（早 ~28s 发布）、1s 场景 cap 的 `end` 必过，被删复检的 saw 写入成为超时
  路径上唯一的差异，`#1392 a started past the helper's cap must not be
  recorded as seen across a timed-out barrier` 判杀（任意 unix 确定性）；
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
- `stranger-pid-reap-removed`（#1409 第 2 条，Linux，绑定名同带模块前缀）：
  pid 模式只留下 kill、删掉 `reap_process` 的 `waitpid` 收割——僵尸仍是本
  进程的子进程，`/proc/<pid>` 条目继续存在，pid 模式用例新增的
  `#1409 the pid-mode guard must reap the stranger` 判杀。组模式早有
  `stranger-reap-removed`，pid 模式此前只有 kill 一个变异体；僵尸有没有被
  收走，唯一的证据就是从 `/proc` 里消失，所以判据直接看条目。
- `reap-evidence-error-always-gone`（#2592 新增，#2142 fix-r3 评审 DS
  sibling #1 转跟进，Linux，绑定单元测试）：pid 模式收割证据的读取错误分
  类退回 fail-open——`Path::exists()` 把任何 `/proc` 读错误都读成「条目已消
  失」，受限 procfs（hidepid / 受限容器 / gVisor）对仍在的 stranger 回
  EACCES 时轮询立即退出、断言在未收割的情况下 PASS（上一行可借道假 PASS
  的形状，与 fix-r3 修掉的 `group_is_gone_within` fail-open 同一形状）。分
  类收口为单一函数 `reap_evidence_is_gone`（复用 `metadata_failure_is_gone`：
  只有 ENOENT 是收割，其余错误视为 stranger 仍在、继续轮询、超时后断言失
  败），绑定新单元测试
  `pid_mode::a_reap_read_error_means_gone_only_when_the_entry_is_absent`（与
  `metadata-failure-always-gone` 绑定的 `a_proc_read_error_…` 同一形态：健康
  宿主的 `/proc` 对后代永远不回错误侧，故钉分类函数本身）的
  `#2592 an unreadable /proc entry means the stranger's state is unknown,
  never reaped`。
- `scan-descriptor-dir-unreadable`（#1409 第 1 条，P1，Linux 契约）：把
  `read_dir` 的 `/proc/<pid>/fd` 改成不存在的名字——修前前两级读失败一律
  `continue`，扫描一条管道都看不见仍返回 Ok，绑定用例照常 PASS；修后自检
  连自己的描述符都看不见，`#1409 reading the descriptors of` 进 dead-helper
  用例的 panic 载荷、挤掉它期待的 `TimedOut`。
- `scan-fd-link-unreadable`（#1409 第 1 条，Linux 契约）：第二级读，把
  `read_link` 的目标改成没人持有的名字——ENOENT 按「fd 已释放」跳过，所以
  这一变异体只能由正向自检判杀：自检的 canary 写端同样看不见，
  `#1409 the return-boundary scan cannot see` 判杀。它与上一条合起来钉住
  「扫描看不见管道就不许 PASS」。
- `boundary-grace-removed`（#1409 第 3 条，Linux，绑定单元测试）：整段宽限
  重检删掉——候选一经发现即算存活写端，扫描与判定之间释放的 fd 会被报成
  泄漏（宽限存在的理由正是这个假 FAIL）；绑定新增单元测试
  `a_candidate_that_no_longer_resolves_is_evicted_by_the_grace` 的
  `#1409 the grace must evict`。
- `boundary-grace-never-evicts`（#1409 第 3 条，Linux，同一单元测试）：
  重检照跑但 `retain(|_| true)` 永不剔除——同一个假 FAIL 换慢路径，同一
  锚点判杀。
- `link-failure-always-release`（#1409 第 1/3 条，Linux，绑定单元测试）：
  `link_failure_is_a_release` 的来源裁决恒为释放——ENOENT 之外任何 errno 都
  当作 fd 已释放。这正是 setuid exec 或 LSM 藏起活 fd 的形状：扫描从没看见
  写端，也照样出干净结论。三处 `read_link`（枚举、fdinfo 复检、宽限复检）走
  同一个函数，一条变异体覆盖三处；绑定
  `an_unreadable_link_is_a_release_only_when_its_holder_is_gone` 的
  `#1409 an unreadable link of this process must never count as a release`。
- `blind-source-settle-always-terminal`（#1409 第 1 条，Linux，绑定单元测试）：
  收尾重查的超时从 Related 改成 Terminal——本进程这类「可读但不终态」的来源
  在 50ms 后被宣判已死、可跳过，宿主藏起后代描述符时未知就变成「那里没有」。
  绑定 `an_unreadable_source_is_skipped_only_when_it_cannot_hold_the_pipe`
  的 `#1409 this process is the scan's own source and must never be skipped`。
- `zombie-leader-is-terminal`（#1409 第 1 条，r2 复审 concurrency，Linux，绑定
  单元测试）：`Z`/`X` 直接当终态，不问线程组是否真死——线程组长
  `pthread_exit()` 后 `Z` 已成立，而共享 fd 表的其余线程仍开着写端，
  `/proc/<tgid>/fd` 恰好在这个状态回 EACCES（cb8 6.17 实测：组长 `Z`、worker
  `S`、fd 目录 EACCES、非阻塞读读端 EAGAIN 即仍有写端，
  `_tmp/20261107-issue-1409/zombie-leader-probe3.log`）。扫描会跳过唯一仍持有
  写端的来源。绑定 `a_live_group_behind_a_zombie_leader_is_not_terminal` 的
  `#1409 a zombie leader with a live group has not released its descriptors`。
  **#2142 起 `Z`/`X` 判据收口为单一函数 `terminal_state`**（枚举首查与收尾重查
  都调它，此前两处各写一遍、可对同一 pid 给出相反结论；production 侧
  `src/test_process.rs` 另有一份同名副本，跨越 crate 边界，fix-r1 起），本变异体
  锚点随迁到本文件该函数本体——把组死检查换成永真第二合取
  （`(pid.is_empty() || true)`，语义仍是「state 单方面决定」）即在**两个入口**
  同时 fail-open；替换文本刻意保留 `pid` 引用，否则 runner 继承的
  `RUSTFLAGS=-D warnings` 宿主会把变异体编译成未用变量失败、整行 INVALID
  （fix-r1 boundary #3）。production 副本另有一条同形变异体钉住（见
  `terminal-state-production-hardcoded-false`），两份判据的函数体各自是
  锚点：任一份漂移即 STALE。
- `thread-group-dead-ignores-live-tasks`（#1409 第 1 条，同上）：判定函数
  `thread_group_is_dead` 对活任务视而不见——只要列出一个任务就判组已死，运行中
  的进程一旦 fd 目录被藏起来就变成可跳过。同一单元测试判杀。
- `terminal-state-hardcoded-false`（#2142 新增，Linux，绑定单元测试）：共享判据
  `terminal_state` 写死永不出终态（`if false &&` 形态，同
  `thread-group-dead-ignores-live-tasks` 先例）——死线程组不再可跳过，扫描对每具
  尸体都 fail-closed；这是此前无变异体钉住的方向（另两个 zombie-leader 变异体
  都朝 fail-open，settle 变异体只钉超时臂）。绑定
  `an_unreadable_source_is_skipped_only_when_it_cannot_hold_the_pipe`（该用例的
  本进程分类**真正走 settle 入口**：自身 related 且非终态，每次
  `blind_source(self)` 都沉入收尾重查；#2142 起它还在同一条断言里以一个已死未
  收割的组钉住**两个入口**的 `Terminal` 判定——单断言元组，变异载荷同时携带两
  个半边的错值，只有两入口真正共用判据时这个判杀才成立）的
  `#2142 a dead thread group must be terminal`。
- `terminal-state-production-hardcoded-false`（#2142 fix-r2 新增，Linux，绑定 lib
  单元测试）：production 扫描（`src/test_process.rs`，#2138）自己的
  `terminal_state` 副本此前没有任何钉子——上面两条变异体只变异 publication 副本，
  production 判据被写死永不出终态（fail-closed 方向：每具尸体都报警）时全仓测试
  照旧全绿，而 #2138 的子进程审计对尸体假报警，无测试发现（sibling r2 #2 /
  boundary r2 #2 / simplify r2 #1，五家评审独立撞上）。本条与
  `terminal-state-hardcoded-false` 同锚点不同文件（`production` 路径，`--lib`）：
  变异后 fail-closed 的是 production 的两个入口；绑定
  `test_process::a_blind_source_is_skipped_only_when_it_cannot_be_a_child`
  （fix-r2 起该用例镜像了 publication 的「已杀未收割、全 `Z` 线程组」元组断言，
  单断言同时钉住枚举首查与 settle 重查的 `Terminal`）的
  `#2142 a dead thread group must be terminal`。绑定用例在共享 lib 测试进程里跑，
  但不持 subreaper 租约、不停毒态（探针是已杀全 `Z` 组，任何扫描都应读作
  terminal），`--exact` 只跑它一个。
- `blind-source-self-unrelated`（#1409 第 1 条，P1 复审补，Linux，绑定
  单元测试）：来源分类短路成「一律 Unrelated」——本进程自己也变成可跳过，
  任何宿主一旦藏起某进程的描述符，扫描都会当成「那里没有」照常出干净结论。
  绑定 `an_unreadable_source_is_skipped_only_when_it_cannot_hold_the_pipe`
  的 `#1409 this process is the scan's own source and must never be
  skipped`；其中「关系进程正在退出」的收尾重查由
  `blind-source-settle-always-terminal` 钉住超时那一臂。窗口量级由实测给出
  （cb8 6.17，`_tmp/20261107-issue-1409/exit-window-probe-3000.log`）：3000 轮
  `/bin/true`、74,584 次 `/proc/<pid>/fd` 读取，14,290 次 EACCES（同刻状态
  `R` 71,504 / `Z` 3,000 / `D` 80），可观测退出窗口 2810 轮，时长 p50 26.7µs、
  p90 38.2µs、p99 48.1µs、max 2.7ms，没有一次达到 50ms 的
  `BLIND_SOURCE_SETTLE`；代码真正失败的原语（逐条 `readlink`）另测一遍
  （`exit-window-probe-readlink.log`：1000 轮同时轮询 fd 目录与 `fd/0` 的
  `readlink`，目录 p50 19.4µs / p99 50.5µs / max 335µs，逐条 `readlink`
  p50 32.0µs / p99 69.0µs / max 315µs，两者都是 0 次达到 50ms）。余量的口径
  要说准：**对 p50/p99 是两三个数量级，对目前观测到的最大样本（2.7ms）是
  18×，且尾部没有被 3000 轮样本界住**（有 2 个样本落在 1–10ms 桶）。不收尾
  这 2810 轮就会变成健康路径上的假 FAIL，收尾过短则在重载下漏掉正在退出的
  后代。
- `probe-group-kill-removed`（#2154 新增，Linux，绑定单元测试）：探针组守卫
  `KillProbeGroupOnDrop` 的组 kill 被删——僵尸组长探针在 `os.fork()` 与打印子
  pid 之间死掉（OOM 形状）时，测试在空 pid 行上 panic，unwind 只剩这个 Drop
  终结毒状态；删掉它，毒组（`Z` 组长 + 活线程组 + EACCES fd 目录）活过
  `SCAN_WINDOW` 的释放。绑定新用例
  `a_probe_that_dies_before_the_pid_line_takes_its_group_with_it`（探针父进程
  不打印直接退出、子进程关掉 stdout 让 EOF 真到达、把自己停进毒状态并把 pid
  发到收据文件）的 `#2154 the poison group must not outlive the guard's kill`。
  **判杀依赖的实际路径时长**（fix-r1，concurrency #1 / boundary #2 收口；fix-r2
  boundary #1 / MM boundary #1 补上最后一段无界等待）：pid 行读取 4s（worker 线程
  + `recv_timeout`，`read_line` 自己没有 deadline）+ 收据等待 10s + 变 Z 等待 10s +
  判杀窗口 5s = 29s，各段有自己的 deadline 断言、超时先红，全在存活线程 30s 上界内
  ——慢宿主上前置循环的时钟吃不掉判杀余量；且 4/10/10/5/30 在测试文件里是命名
  常量（`PROBE_HOLD_SECS` 经 `format!` 注入两段探针脚本），余量算式是**编译期
  assert**（`PROBE_LINE_BUDGET + PROBE_RECEIPT_BUDGET + PROBE_ZOMBIE_BUDGET +
  PROBE_KILL_WINDOW < PROBE_HOLD_SECS`），不再靠注释手算；
  窗口超时分支另有一行**独立于锚点**的显式 killpg + 有界确认（先确认组在死、
  再 panic 放窗口，concurrency #2；fix-r3 concurrency P1 把确认升为真断言——首确认
  失败再 killpg 一次并按 `PROBE_DEATH_RETRY_WINDOW` 延长一轮，仍失败才以专属消息
  panic，确认结果不再被 `let _ =` 丢弃），把变异体的盘上残留当场收掉。
  真实组长探针的子进程同样首句 `os.close(1)`（fix-r1 boundary #1，实测
  `_tmp/20261026-issue-2142/fix-r1/oom-eof-probe-ab.log`：不关 fd 1 时 OOM 杀掉
  父进程后 read_line 要等存活线程满 30s 才见 EOF——「空 pid 行 panic」的 OOM
  机理此前对真实探针不成立），此后该机理对两个探针同形。探针各自成组
  （`process_group(0)`），组长用例的健康路径仍先按 pid 精确 kill
  （`KillOnDrop`），组守卫只是断后。
- `metadata-failure-always-gone`（#2142 fix-r3 新增，Linux，绑定单元测试）：
  `group_is_gone_within` 的 `/proc` 读取错误分类退回 fail-open——任何错误都当
  「条目已消失」。受限 procfs（hidepid / 受限容器）对活 hero 回 EACCES 时，判杀
  确认会把一个看不见的活组宣判为死（`probe-group-kill-removed` 可借道假 PASS 的
  形状）。分类收口为单一函数 `metadata_failure_is_gone`（只有 ENOENT 是消失，
  其余错误视为组仍存活），绑定新单元测试
  `a_proc_read_error_means_gone_only_when_the_entry_is_absent`（与
  `an_unreadable_link_is_a_release_only_when_its_holder_is_gone` 同一形态：健康
  宿主的 `/proc` 对后代永远不回错误侧，故钉分类函数本身）的
  `#2142 an unreadable /proc entry means the group's state is unknown, never gone`。
- `group-gone-always-false`（#2142 fix-r3 新增，Linux，绑定 leader 单元测试）：
  判杀确认的裁决写死永假——「`/proc` 永远无法证明死亡」的形状（fix-r3 P1 修的
  正是确认结果曾被 `let _ =` 丢弃）。绑定
  `a_live_group_behind_a_zombie_leader_is_not_terminal`：变异下首等判杀窗口即假、
  进超时分支，显式 killpg 后首确认假、升级轮（再 killpg +
  `PROBE_DEATH_RETRY_WINDOW`）的 assert 以专属消息失败——对修前代码同一变异只会
  打出分支 panic 的旧消息、本行标记不出现，所以这条标记就是「断言存在且在路径
  上」的可杀事实。判杀消息
  `#2142 the timeout branch must not panic over a group whose death it could not confirm`。

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
# 锚点新鲜度预检（须打印 `<cases 条数> anchors: ok`，并打印派生的 `mutants:` /
# `linux-contract:` 两行；README 末尾两个标记与派生值逐项一致才退出 0）
python3 -I negative_controls.py --check-anchors
```

每个变异体自带 cargo target：ETXTBSY 竞态变异体跑 `--lib`（绑定 grep
压测），其余跑 `--test scratch_publication`；两类的条数见 `mutants` 标记的
`lib` / `test` 两项。

平台契约：macOS 的 std::process 走 posix_spawn，观察不到继承描述符窗口，
`script-direct-write` / `helper-in-process-write` 在该平台预期 SURVIVED
（或因无关失败 INVALID）——这两个竞态负控是 **Linux 契约**，判杀只在
Linux 宿主（cb8 / CI）上成立。其余 Linux 契约变异体分两类，**非 Linux 上
的预期结果不同**（#1409 第 4 条更正：此前一段把它们一律写成 SURVIVED）：
① `SURVIVES_OFF_LINUX`——被 cfg 掉的是扫描或收割的代码路径，绑定用例在
非 Linux 上仍然存在且照常通过，宿主观测到 **SURVIVED**；②
`INVALID_OFF_LINUX`——绑定的是 `#[cfg(target_os = "linux")]` 的单元测试或
`pid_mode` 专属用例，非 Linux 上跑 `--exact` 匹配 0 个测试，正控就被
`passed()` 拒，宿主观测到 **INVALID**（不是 SURVIVED）。两份名单不在这里
手抄，正本就是 `negative_controls.py` 里的同名元组；条数与名单由 `cases` +
这两个集合派生，`--check-anchors` 把它与下面两个标记逐项比对。不在契约里的
变异体在任意 unix 宿主上确定性判杀（非 unix 无该测试模块）。
cargo 不得带 `-q`（共享执行器靠逐测试行判读）。

<!-- mutants: total=41 lib=3 test=38 unix=16 linux-survives=9 linux-invalid=16 -->
<!-- linux-contract: script-direct-write helper-in-process-write boundary-write-end-filter-removed boundary-fdinfo-read-error boundary-fdinfo-parse-error boundary-fdinfo-unknown-passed stranger-subreaper-removed stranger-reap-removed stranger-pid-kill-removed stranger-pid-reap-removed reap-evidence-error-always-gone scan-descriptor-dir-unreadable scan-fd-link-unreadable boundary-grace-removed boundary-grace-never-evicts link-failure-always-release blind-source-settle-always-terminal zombie-leader-is-terminal thread-group-dead-ignores-live-tasks terminal-state-hardcoded-false terminal-state-production-hardcoded-false blind-source-self-unrelated probe-group-kill-removed metadata-failure-always-gone group-gone-always-false -->

