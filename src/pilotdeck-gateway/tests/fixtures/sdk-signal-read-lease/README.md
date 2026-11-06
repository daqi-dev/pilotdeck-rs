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
  窗口标记、睡 `WINDOW_SECONDS` 后恢复放锁），控制方等窗口标记后经 `run_owned` 跑
  常驻测试，期望它等到 writer 恢复才读盘并 PASS。控制方全程不持树锁，无死锁。
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

### GREEN 的竞争见证（`CONTENTION_FLOOR`）

GREEN 不能只看「测试通过」——没等到 writer 也会通过。所以 GREEN 还要求：

1. 预编译（`--no-run`）之后再跑测试，`cargo test --exact` 的启动开销是秒级；
2. `run_owned` 内实测耗时 ≥ `CONTENTION_FLOOR`（5 s）。真等到租约的测试不可能早于
   writer 放锁完成（窗口本身 60 s），低于下限即「从未竞争」，记 INVALID 并提示在
   cargo 名额空闲时重跑。

这消除了「弱证据通过」：以前名额排队导致测试晚于窗口到达时会被记成 KILLED，现在记
INVALID（fail-closed）。耗时只量 `run_owned`，`wait_for_slot` 的排队时间不计入。

### writer 的有界退出

writer 的 `mutating_sources` 用 `lock_timeout=WRITER_LOCK_TIMEOUT`（300 s）快失败：
抢锁慢说明工作区忙，不是租约的证据，这个控制项也不该成为最长持锁者。

停止 writer 分三级：先等它自己恢复完成（`WRITER_EXIT_BUDGET = 窗口 + 120 s`）；超时
再发 `SIGTERM`，writer 的处理器抛 `SystemExit` 让 `mutating_sources.__exit__` 把每个
guarded 文件恢复回去；再超时才 `_kill_group`（复用 `negative_process` 的实现）并
`_collect` 有界收尾。只有真被 SIGKILL 才会留变异体，下一次 `mutating_sources` 会用
HEAD 字节校验拒绝脏树，不会把变异体当基线。`finally` 里的停止错误会作为 INVALID
原因上报，不覆盖主结果。

## 运行（提交后；mutating_sources 拒绝脏树）

```sh
CARGO_TARGET_DIR=<target> python3 -I \
  src/pilotdeck-gateway/tests/fixtures/sdk-signal-read-lease/negative_controls.py NEW_OUTPUT_DIR
```

GREEN 依赖 writer 窗口（默认 60 s）覆盖常驻测试从启动到取锁的耗时；若宿主 cargo
名额排队导致测试晚于窗口到达，GREEN 记 INVALID（竞争下限未达标），不再记成 KILLED。
RED 仍是决定性方向。
