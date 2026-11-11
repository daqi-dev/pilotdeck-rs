# sdk-signal-read-lease 负控（#1662）

#1662 的成对负控：常驻 `sdk_signal_exit` 金样测试的读租约（`fixture_read_lease`，
对 `<root>/.negative-source.lock` 取共享 `flock`）。

## 背景

`session-lease/negative.py` 曾把 `capture.py` 列入 guarded sources，committed 模式在
`mutating_sources` 内原地改写；同一树上 `signal_oracle_matches_recorded_source_and_local_files`
无锁读盘算 sha256 → 并发假 FAIL；Python 侧 SIGKILL 后变异体留盘 → 持续红。修复分两半：

1. session-lease 守卫集移除 `capture.py`，capture 案例改 `--memory-run` 注入（见
   `session-lease/`，其自带 26 项案例含两个 #1662 变异）。
2. `sdk_signal_exit.rs` 的读盘测试统一持共享读租约；#1885 起该租约实现下沉到
   `src/pilotdeck-cli/tests/common/read_lease.rs`（本负控的对象，`test_source`；gateway 侧
   `src/pilotdeck-gateway/tests/common/read_lease.rs` 是另一份镜像，不在本控制射程内）。

## 三段式判杀（GREEN / RED / writer-exit-contract）

- **GREEN `fixed-test-survives-writer-window`**：预编译后起独立 writer 子进程
  （`--writer`：自身走 `mutating_sources(root, [capture.py])`，持锁写变异、原子发布
  窗口标记与 `writer-hold.json`，持锁 `HOLD_SECONDS`（90 s）后恢复放锁），控制方等
  窗口标记后经 `run_owned` 跑常驻测试，期望它等到 writer 恢复才读盘并 PASS。控制方
  全程不持树锁，无死锁。

  **见证绑定到本次 writer**：writer 在**放锁前**把持锁区间
  （`start_ms` / `end_ms`，epoch 毫秒）原子写进 `writer-hold.json`；常驻测试在第一次
  **被拒绝**（`try_lock_shared` 返回 `WouldBlock`）时记下时刻
  `blocked=<epoch ms>`，marker 里同时带 `first`（第一次尝试）、`blocked` 与 `at`
  （取锁成功）。GREEN 要求 `start_ms ≤ blocked ≤ end_ms − 1000`（`WITNESS_MARGIN_MS`，
  只加在上端，理由见下文「边界毫秒」）。

  `WouldBlock` 是「此刻有排他持有者」的直接观测：进程里没有别的东西能造出它，而本次
  writer 在整个持锁区间内一直持有排他锁，所以落在区间内的拒绝只能来自它。用等待时长
  或「第一次尝试时刻」作见证都不够——调度暂停可以把这两者挪到没遇到本次 writer 的时刻
  （暂停前记时刻、暂停后调用；或无竞争取锁后再暂停把 0 等待刷大），而 `blocked` 是
  调用返回值的伴生时刻，暂停挪不动它。

  cargo 名额在起 writer **之前**取（`cargo-slots.sh` 是查询式），所以 90 s 的持锁
  只需要覆盖测试本身，不必覆盖最长 900 s 的名额排队。
- **RED `lease-removed-false-fail`**：`mutating_sources(root, [common/read_lease.rs,
  capture.py])` 内把租约的持锁者等待（`thread::sleep(Duration::from_millis(50));`）
  换成 `return Some(file);`（变异体先经 `cargo test` 隐式编译验证），capture.py 追加变异行，
  持锁跑同一测试 → 期望 FAIL 于
  `#1145 source file SHA256 matches disk: probeSha256/capture.py`。

- **`writer-exit-contract`**（`--stop-check`）：两跑，两跑本身不跑 cargo（冷 target 会先
  bootstrap 一次工作区扫描器、占一次名额），证明 `stop_writer()` 的两条判据真的成立——
  被叫停的 writer 恢复并退出 0，自己失败的 writer 非零且被报出。细节见下文
  「writer 的退出契约自检」。

三行全 KILLED 才退出 0；锚点过期在写盘前报错退出（INVALID，不记 SURVIVED）。全部
cargo 经 `sigint_default.py` 包装、`run_owned` 进程组管理、`wait_for_slot` 名额闸。

GREEN 与 RED **不共用判分器**。RED 侧「金样 sha 断言失败」就是变异被杀（KILLED）；GREEN 侧同
一失败意味着修复失效，因此 GREEN 只接受 **PASS**：任何失败（编译错、错 panic、金样
sha 失败）一律 INVALID，绝不会被记成 KILLED。

### GREEN 的竞争见证（测试自报等待时长）

GREEN 不能只看「测试通过」——没等到 writer 也会通过；也不能拿 cargo 的总耗时当见证
（里面含启动、`~/.cargo` 包缓存锁与重编译，既可能被无关等待刷大，也可能把真实但很短
的等待判小）。所以见证来自测试自己：

- `fixture_read_lease()` 在拿到共享锁时打一行 stderr：
  `#1662 read lease wait ms=<n> first=<epoch ms> blocked=<epoch ms> at=<epoch ms>`
  （等待时长 + 第一次尝试 + 第一次被拒 + 取锁时刻；`blocked=0` 表示第一次就成功）。
- GREEN 用 `--nocapture` 跑常驻测试，解析这一行；没有该行、或 `blocked=0` →
  INVALID（从未被拒，即从未遇到排他持有者）；`blocked` 不在 writer 公布的持锁区间内 →
  INVALID（拒绝不是本次 writer 造成的）；等待 `< MIN_LEASE_WAIT_MS`（500 ms）→ INVALID。
- 派生量守卫：公布的持锁跨度 `end_ms − start_ms` 不超过 `WITNESS_MARGIN_MS`（1000 ms）时
  直接 INVALID。上端边距会吃掉整个区间，任何拒绝都进不去，剩下的只有一句误导人的
  「拒绝不在本次 writer 区间内」；把 `HOLD_SECONDS` 误写成旁边的
  `STOP_CHECK_HOLD_SECONDS` 正好会落进这一格（r9/r10 P3）。
- 只有「测试 PASS + `blocked` 落在本次 writer 的持锁区间 + 等待 ≥ 500 ms」才是 KILLED。
- marker 字面量漂移守卫：`LEASE_MARKER` 不在 `common/read_lease.rs` 文本里时 GREEN 直接
  INVALID，避免两侧改名后控制项静默失效。

prewarm（`--no-run`）返回非 0 直接 INVALID：否则第二次运行会在计时窗口里重编译。
起 writer 之前还会先 `_ensure_workspace_scanner()` 预热 #1166 工作区扫描器 —— 否则
`mutating_sources` 在持锁体内构建它，冷 target 上会吃掉 writer 的入口预算。

### writer 的有界退出

writer 的 `mutating_sources` 用 `lock_timeout=WRITER_LOCK_TIMEOUT`（300 s）快失败：
抢锁慢说明工作区忙，不是租约的证据，这个控制项也不该成为最长持锁者。

停止 writer 分三级：先等它自己恢复完成（`WRITER_EXIT_BUDGET = HOLD_SECONDS + 120 s`）；
超时再发 `SIGTERM` —— writer 的处理器**只在窗口期内生效**，抛私有 `_Stopped` 让
`mutating_sources.__exit__` 把每个 guarded 文件恢复、重读校验后再放锁；再超时才
`_kill_group`（复用 `negative_process` 的实现）并 `_collect` 有界收尾。

**退出码无条件作数**：被我们 `terminate()` 的 writer 走的是「恢复并复核 → 退出 0」，
所以 `stop_writer()` 对任何非零 rc 都报失败——恢复抛错、HEAD 复核失败、或信号还没发出
writer 已自行失败退出，全都会被看见（`#1662` r8 P1：先前在 `terminate()` **之前**置
`terminated` 标志，把这类失败一并吞掉）。真被强杀则明写
「source restoration is NOT confirmed」。

**解除武装在抛异常之前**，并且放在覆盖整个窗口的 `try/finally` 里：无论窗口是正常
结束、超时还是被 SIGTERM 打断，恢复阶段（`write_bytes`）都已在无处理器状态下运行，
第二枚 SIGTERM 不会中途打断恢复。停止走 `finally`（捕获 `BaseException`），所以
`Ctrl-C` / `SystemExit` 同样会停掉 writer，不会留下持锁的孤儿子进程。

停止本身若被中断（`communicate()` 期间 `Ctrl-C`），`force_stop()` 仍然先 `terminate()`
走优雅恢复、等 `WRITER_EXIT_BUDGET`；只有**确认超时**才 `_kill_group`，且 `_collect`
放在 `finally` 里一定会执行。确认口径：`communicate()` 抛 `TimeoutExpired`；或它在第一次
等待时就抛了别的异常（IPC / 管道问题）——`wait()` 会重复同样的抛错，故不再尝试，直接用
不会抛错的 `poll()` 轮询到同一期限，到点仍在跑才算确认超时。`force_stop()` 现在也按
**退出码**回答（`returncode == 0` 才算干净停下）：writer 被请求停止时走「恢复并复核 →
退出 0」，所以非零 rc 意味着它没走完这条路，调用方不能声称恢复成功（r9/r10）；只有真被
强杀或非零退出时，该行 reason 才明写「source restoration is NOT confirmed」。中断本身记在
`hard_interrupt` 里，不会被改写后的 `stop_error` 顶掉——**兜底自身再抛错时也一样**，仍
照原样重抛；兜底抛的是普通异常时，其文本也拼进 reason，不静默丢弃。

**为什么边距之后 `blocked` 仍只能来自本次 writer**：`end_ms` 是在
`mutating_sources` 的 `with` **体内**、`__exit__` 恢复之前取的，所以真实放锁时刻必然
晚于 `end_ms` 的采样。但 `epoch_ms()` 向下取整，一个数值代表整毫秒桶：writer 在
`N+0.1 ms` 采样得 `end_ms=N`、`N+0.2 ms` 放锁、另一个排他持有者 `N+0.3 ms` 取锁、
读者 `N+0.4 ms` 被它拒绝也记 `N`，而 writer 的 `finally` 延迟到 `N+1.1 ms` 才记
`released_ms=N+1` —— 旧的 `start ≤ blocked ≤ end` 与 `released > end` 都成立，却把
别人的拒绝记成本次 writer 的（r8 P0）。因此 GREEN 要求 `blocked ≤ end_ms − 1000`：
这意味着真实拒绝时刻比真实放锁至少早 1 s，而这段时间里本次 writer 一直持着排他锁，
`flock` 不允许第二个排他持有者存在。1 s 只占 90 s 持锁的约 1 %，远大于任何调度抖动
（r8 实测 `blocked` 距 `start_ms` 1.4 s、距 `end_ms` 88.6 s）。

**下界不留边距**（r9 P1）：`start_ms` 在 `with` 体内采样，此时 `mutating_sources`
已经持锁且变异已写盘，不可能有别的持有者早整整一个毫秒桶；下界再加 1 s 只会吃掉读者
的启动余量（r8 实测 `blocked − start_ms ≈ 1.4 s`），把正常拒绝判成 INVALID。

### writer 的退出契约自检（`--stop-check`）

`stop_writer()` 的「退出码无条件作数」是可复现的，不是注释里的断言：两跑，两跑本身不
跑 cargo（冷 `CARGO_TARGET_DIR` 上仍会先 `_ensure_workspace_scanner()` bootstrap 一次
#1166 工作区扫描器并占 cargo 名额，预热过就只做两跑）。

```sh
CARGO_TARGET_DIR=<target> python3 -I \
  src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py \
  --stop-check NEW_OUTPUT_DIR
```

- `NEW_OUTPUT_DIR/sigterm/`：writer 持锁 `STOP_CHECK_STOP_HOLD_SECONDS = 30 s`，控制方用
  `stop_writer(..., budget=STOP_CHECK_STOP_BUDGET = 5 s)` 停它 —— 第一段等待必然超时，
  于是走的是**控制方自己的 terminate 分支**（手动 `terminate()` 会让这条分支永远没人
  跑）。要求：窗口已发布（`window=True`）、`stop_writer` 不抛错、rc 0、字节回位、
  `restored: true`、stderr 里出现 `#1662 writer stopped`，**且**
  `released_ms − start_ms < 30 s`（停止确实由信号触发，不是睡满 hold 自己退出）。
- `NEW_OUTPUT_DIR/own-failure/`：`PDRS_1662_WRITER_FAIL_AFTER_HOLD=1` 让 writer 正常
  跑满 `STOP_CHECK_HOLD_SECONDS = 5 s` 窗口后自行失败。要求窗口已发布（`window=True`）、
  rc 非 0 且非 None、`stop_writer()` 抛错且报出的文本含该退出码（正则整词匹配，
  `exited 1` 不会被 `exited 10` 命中）、字节回位。等待预算用
  `STOP_CHECK_EXIT_BUDGET = STOP_CHECK_HOLD_SECONDS + STOP_CHECK_RESTORE_ALLOWANCE_SECONDS`
  （当前 60 s）而不是生产的 `WRITER_EXIT_BUDGET`：这个 writer 5 s 后就自己走，等待只需覆盖
  恢复 + HEAD 复核；预算由 hold 派生而不是字面量，抬高 hold 时不会反而把第一段等待变成
  「健康 writer 还在睡就被 SIGTERM」（那样它会优雅退出 0，own-failure 这条覆盖静默失效）。
- **预算是「每段等待」**：`stop_writer()` 有两次等待（第一次等它自己走、终止后再等一次），
  兜底 `force_stop()` 另有两次；坏 writer 挂在恢复里时最坏是
  `2 × STOP_CHECK_EXIT_BUDGET + _collect(30 s)`，再加快底的一段，不是 60 s 整跑上限
  （r12 concurrency/boundary P3）。自检的兜底也吃这个短预算，不走生产的
  `WRITER_EXIT_BUDGET`（`HOLD_SECONDS + 120`）。
- 两跑任一行的兜底 `force_stop()` 若**自身被中断**（例如第二枚 Ctrl-C 落在等待里），
  该异常会记进该行的 `cleanup` 字段并让整行判 INVALID：writer 可能仍持锁、恢复未确认，
  不能当作干净收场上报（r10 P1）。
- **未覆盖**：`#1662 writer stopped` marker、`own-failure` 的 `bytes_back`、`rc is None`
  与退出码整词匹配这四条断言只有正控路径——仓内没有能造出「忽略 SIGTERM、跑满 hold 后
  正常退出」「恢复失败但退出 0」「writer 永不 reap」的 writer，它们从没红过。补注入开关
  的负控记在 #1885，前置于 #1662 收尾（r10 P2 / r12 sibling P2）。

原始输出形如：

```
sigterm: rc=0 window=True bytes_back=True released=(1793973366834, True) reported=None
own-failure: rc=1 window=True bytes_back=True released=(1793973381421, True) reported='#1662 writer exited 1'
```

这一步已经挂进默认执行清单（见「运行」）：默认三行是 GREEN、RED、`writer-exit-contract`，
任一行非 KILLED 都退出 1。（r9 P2：自检模式若无人调用就是死代码。）自检中断时走
`force_stop()` 的优雅停止 + 有界等待，**不以 SIGKILL 起手**——恢复途中的强杀会把变异体
留在盘上（r9 P0）。

这一点不靠论证：writer 在 `with` 之外的 `finally` 里（`__exit__` 已恢复并放锁）原子
发布 `writer-released.json`（`released_ms` 与 `restored`；**仅当窗口已进入时发布**——
取锁阶段被 SIGTERM 的 writer 从未持锁，不公布假证据；窗口进入后无论正常结束还是 SIGTERM
展开都发布），GREEN 要求 `released_ms > end_ms`，否则 INVALID（区间不保守）；**记录缺失
同样 INVALID**（缺证据不算证据，不允许静默跳过这条检查），KILLED 的 reason 里也带上该
时刻。每次运行因此都留下「公布区间严格落在真实持锁区间内」的实测记录，而不是注释里的
推断。

`restored` 是 writer 放锁后对 guarded 文件的**直接观测**（`read_bytes()` 与
`git show HEAD:<file>` 的字节比对，`capture_matches_head()`），GREEN 要求
`restored: true`；缺失或 false 一律 INVALID —— 「放锁记录存在」不再等于「恢复成功」
（r8 concurrency/boundary P2）。

wall clock 被外部改写（NTP 步进 / 手工改表）不在该模型的守卫范围内：两侧都用
`time.time()` 打点，时钟跳变会让跨进程的时刻比较失去意义；本条记在 #1885。

SIGKILL、或恢复本身失败（磁盘满 / 权限），仍会留下变异体；下一次 `mutating_sources`
用 HEAD 字节校验拒绝脏树，不会把变异体当基线，但源文件需要人工恢复。
主错误与停止错误**都**写进 INVALID 的 reason（`主错误; stop failed: …`），互不遮盖。

## 窗口重定向与宿主预持（#2513 fix-r7）

本 fixture 与 `sdk_signal_exit` 矩阵同车道：宿主按
`src/pilotdeck-cli/tests/fixtures/sdk-signals/README.md` 的对照表先取
`/tmp/pdrs-parity.lock` 再跑整批。而 `src/pilotdeck-cli/tests/` 自 #2513 起是
parity guarded 前缀，`writer()` / `red()` / `--stop-check` 的变异窗口覆盖的
`capture.py` 命中该前缀 → `mutating_sources` 走 `_parity_mutation_lock`，在宿主
持有的共享锁上排队 60 s 超时：GREEN、RED、SIGTERM 收尾全部假 INVALID
（fix-r6 给 8 个 sdk fixture 加 `PDRS_SDK_TEST_PARITY_LOCK` 重定向时漏掉了本
fixture，fix-r7 补齐）。

修复是 sdk-signals fix-r4 的 Python 同型：`_redirect_window_parity_lock` 把
`negative_process._parity_mutation_lock` 换成重定向版——窗口租约取在证据目录的
`out/parity-redirect.lock` 上，对宿主预持的共享租约不互斥；干净版 `common.py`
仍从 HEAD 证明的字节 staged 执行（绝不变异盘上代码）。`_require_host_lease`
前置闸在窗口打开**之前**跑：共享租约的 owner.json 必须在场且是活的非本进程 pid
（宿主预持的证明），否则一行原因退出 2——「取不到租约即退出稍后重跑」，不是
无守卫地开窗口。writer 的重定向安装在最外层 `try` 之前、恢复放在最外层
`finally` 的**第一项**（它补的是模块全局，留下会误导本进程后续窗口）。

`sdk_signal_exit.rs` 本身**不取** parity 租约（只有 `.negative-source.lock` 读
租约），Rust 侧无需重定向。

两个新自检模式（都不跑 cargo，不占名额）：

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py \
  --window-lock-check NEW_OUTPUT_DIR
python3 -I src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py \
  --kill-window-lock NEW_OUTPUT_DIR
```

- **`--window-lock-check`**：共享租约被一个活的外部进程持有（有宿主预持就用它，
  否则自起一个有界 `--shared-hold` 持有者，SIGTERM 干净放锁）时，guarded 入口必须
  在 `WINDOW_LOCK_PROBE_SECONDS`（5 s）界内于重定向路径取到窗口锁：owner.json 在
  `out/parity-redirect.lock` 上、探针不阻塞、窗口退出后重定向租约按协议释放。这是
  kill 行钉住的条件——重定向失效时探针在共享租约上排队，5 s 后带着
  「#2513 该 fixture 必须能取到窗口锁」的断言失败，不是 60 s 挂起。
- **`--kill-window-lock`**：一条自变异 kill 行。锚点
  `lock=out / "parity-redirect.lock"):` 改回 `lock=common.LOCK):`，变异体经
  `--window-lock-check` 子进程判定：KILLED 要求失败日志带该行专属 marker（排他：
  同入口的无关红仍是 INVALID，绝不记 KILLED）；正跑、变异跑、恢复跑各有产物；
  非 KILLED 退出非 0。与 `--stop-check` 同理须整批提交后跑
  （`mutating_sources` 拒绝脏树）。

## 运行（提交后；mutating_sources 拒绝脏树）

```sh
CARGO_TARGET_DIR=<target> python3 -I \
  src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
```

默认执行清单三行：`fixed-test-survives-writer-window`（GREEN）、
`lease-removed-false-fail`（RED）、`writer-exit-contract`（`--stop-check` 的两跑，
产物在 `NEW_OUTPUT_DIR/sigterm/` 与 `NEW_OUTPUT_DIR/own-failure/`）。三行全 KILLED
才退出 0。

若宿主 cargo 名额排队导致测试晚于 writer 放锁才到达，测试自报的等待为 0（或 `at`
落在持锁区间之外），GREEN 记 INVALID，不会记成 KILLED。RED 仍是决定性方向。
