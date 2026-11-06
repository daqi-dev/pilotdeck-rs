# sdk-signal-read-lease 负控（#1662）

#1662 的成对负控：常驻 `sdk_signal_exit` 金样测试的读租约（`fixture_read_lease`，
对 `<root>/.negative-source.lock` 取共享 `flock`）。

## 背景

`session-lease/negative.py` 曾把 `capture.py` 列入 guarded sources，committed 模式在
`mutating_sources` 内原地改写；同一树上 `signal_oracle_matches_recorded_source_and_local_files`
无锁读盘算 sha256 → 并发假 FAIL；Python 侧 SIGKILL 后变异体留盘 → 持续红。修复分两半：

1. session-lease 守卫集移除 `capture.py`，capture 案例改 `--memory-run` 注入（见
   `session-lease/`，其自带 26 项案例含两个 #1662 变异）。
2. `sdk_signal_exit.rs` 的读盘测试统一持共享读租约（本负控的对象）。

## 两段式判杀

- **GREEN `fixed-test-survives-writer-window`**：预编译后起独立 writer 子进程
  （`--writer`：自身走 `mutating_sources(root, [capture.py])`，持锁写变异、原子发布
  窗口标记与 `writer-hold.json`，持锁 `HOLD_SECONDS`（90 s）后恢复放锁），控制方等
  窗口标记后经 `run_owned` 跑常驻测试，期望它等到 writer 恢复才读盘并 PASS。控制方
  全程不持树锁，无死锁。

  **见证绑定到本次 writer**：writer 在**放锁前**把持锁区间
  （`start_ms` / `end_ms`，epoch 毫秒）原子写进 `writer-hold.json`；常驻测试在
  **第一次尝试取锁之前**记下时刻 `first=<epoch ms>`，marker 里同时带取锁时刻
  `at=<epoch ms>`。GREEN 要求 `start_ms ≤ first ≤ end_ms`：第一次尝试就落在本次
  writer 的持锁区间内，说明读者确实撞上了它。用 `first` 而不是等待时长作见证，是为了
  排除两条假通过路径——「本次 writer 放锁后另一个 committed 任务先抢到锁、读者等的是
  它」和「读者无竞争取锁后被调度暂停 ≥ 500 ms」。

  cargo 名额在起 writer **之前**取（`cargo-slots.sh` 是查询式），所以 90 s 的持锁
  只需要覆盖测试本身，不必覆盖最长 900 s 的名额排队。
- **RED `lease-removed-false-fail`**：`mutating_sources(root, [sdk_signal_exit.rs,
  capture.py])` 内把租约的持锁者等待（`thread::sleep(Duration::from_millis(50));`）
  换成 `return file;`（变异体先经 `cargo test` 隐式编译验证），capture.py 追加变异行，
  持锁跑同一测试 → 期望 FAIL 于
  `#1145 source file SHA256 matches disk: probeSha256/capture.py`。

两段全 KILLED 才退出 0；锚点过期在写盘前报错退出（INVALID，不记 SURVIVED）。全部
cargo 经 `sigint_default.py` 包装、`run_owned` 进程组管理、`wait_for_slot` 名额闸。

两段**不共用判分器**。RED 侧「金样 sha 断言失败」就是变异被杀（KILLED）；GREEN 侧同
一失败意味着修复失效，因此 GREEN 只接受 **PASS**：任何失败（编译错、错 panic、金样
sha 失败）一律 INVALID，绝不会被记成 KILLED。

### GREEN 的竞争见证（测试自报等待时长）

GREEN 不能只看「测试通过」——没等到 writer 也会通过；也不能拿 cargo 的总耗时当见证
（里面含启动、`~/.cargo` 包缓存锁与重编译，既可能被无关等待刷大，也可能把真实但很短
的等待判小）。所以见证来自测试自己：

- `fixture_read_lease()` 在拿到共享锁时打一行 stderr：
  `#1662 read lease wait ms=<n> first=<epoch ms> at=<epoch ms>`
  （取锁前后 `Instant` 实测 + 第一次尝试时刻 + 取锁时刻）。
- GREEN 用 `--nocapture` 跑常驻测试，解析这一行；没有该行 → INVALID（从未等过），
  `first` 不在 writer 公布的持锁区间内 → INVALID（等的不是本次 writer），
  等待 `< MIN_LEASE_WAIT_MS`（500 ms）→ INVALID。
- 只有「测试 PASS + 第一次尝试落在本次 writer 的持锁区间 + 等待 ≥ 500 ms」才是 KILLED。
- marker 字面量漂移守卫：`LEASE_MARKER` 不在 `sdk_signal_exit.rs` 文本里时 GREEN 直接
  INVALID，避免两侧改名后控制项静默失效。

prewarm（`--no-run`）返回非 0 直接 INVALID：否则第二次运行会在计时窗口里重编译。
起 writer 之前还会先 `_ensure_workspace_scanner()` 预热 #1166 工作区扫描器 —— 否则
`mutating_sources` 在持锁体内构建它，冷 target 上会吃掉 writer 的入口预算。

### writer 的有界退出

writer 的 `mutating_sources` 用 `lock_timeout=WRITER_LOCK_TIMEOUT`（300 s）快失败：
抢锁慢说明工作区忙，不是租约的证据，这个控制项也不该成为最长持锁者。

停止 writer 分三级：先等它自己恢复完成（`WRITER_EXIT_BUDGET = HOLD_SECONDS + 120 s`）；
超时再发 `SIGTERM` —— writer 的处理器**只在窗口期内生效**，抛 `SystemExit` 让
`mutating_sources.__exit__` 把每个 guarded 文件恢复回去；再超时才 `_kill_group`（复用
`negative_process` 的实现）并 `_collect` 有界收尾。

**解除武装在抛异常之前**，并且放在覆盖整个窗口的 `try/finally` 里：无论窗口是正常
结束、超时还是被 SIGTERM 打断，恢复阶段（`write_bytes`）都已在无处理器状态下运行，
第二枚 SIGTERM 不会中途打断恢复。停止走 `finally`（捕获 `BaseException`），所以
`Ctrl-C` / `SystemExit` 同样会停掉 writer，不会留下持锁的孤儿子进程。

SIGKILL、或恢复本身失败（磁盘满 / 权限），仍会留下变异体；下一次 `mutating_sources`
用 HEAD 字节校验拒绝脏树，不会把变异体当基线，但源文件需要人工恢复。
主错误与停止错误**都**写进 INVALID 的 reason（`主错误; stop failed: …`），互不遮盖。

## 运行（提交后；mutating_sources 拒绝脏树）

```sh
CARGO_TARGET_DIR=<target> python3 -I \
  src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
```

若宿主 cargo 名额排队导致测试晚于 writer 放锁才到达，测试自报的等待为 0（或 `at`
落在持锁区间之外），GREEN 记 INVALID，不会记成 KILLED。RED 仍是决定性方向。
