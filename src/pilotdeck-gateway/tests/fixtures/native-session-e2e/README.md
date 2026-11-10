# native-session-e2e 端到端控制（#1140）

本套件用真实 loopback socket mock 网关和 provider，OS 的 strace/pidfd/platform 为确定性桩。它验证采集器、认证与评分接线；不证明真实 Rust 的全部场景行为，刺激对齐仍由 #1242 接续。

`mock_gateway.py` 按登记期望发 event/seq/final 信封，写带项目身份的 `.cwd`、转录与 registry；`test_e2e.py` 经采集器走原始附件、receipt、冻结认证和评分。被测的评分投影负控在先认证健康输入后单独改观测，避免前置认证拒收掩盖评分分支；认证负控则重钉 receipt，完整走 loader。逐案 PASS 与整批 NOT_RUN 分开断言。

端口在受控的 gateway 28999..29008 系列中成对探测，并预留 gateway 与下一 provider 端口。provider 实际绑定前必须释放预留，因此第三方抢占会让本样本失败；不借用用户进程。并行副本验证各自取不同端口对。

`negative_controls.py` 的 `cases` 与执行汇总是变异列表和数量的唯一来源（#1535 起登记独立总数 `EXPECTED_CASES`，在 `main()` 入口与 `preflight()` 首行双点位先验，行数漂移立即拒绝启动）。每项绑定完整测试名与专属失败消息，先 compile，再 positive → mutant → restored，全部跑完汇总。原地模式用共享 mutating_sources/run_owned，并要求被测源码与 HEAD 一致；新冻结文件提交之前由协调者在提交后运行。#1402 起原地模式按 case 粒度取/还全局 parity 租约：等槽在租约外；#1536 起正控移入租约窗（正控读受管字节——夹具加载 collect.py 与冻结 session_domain.py，正控期间异树不可写字节）；租约窗口覆盖变异体在盘全程，组成为正控 + cargo no-run 预检 + negative + restored 复跑（restored 也在租约内），租约界为 BUILD_SECONDS + 2 * BOUND_TEST_SECONDS（低于 stale 上限 MAX_AGE），case 间空窗仍允许第二台宿主取走租约；超时与取消路径同样释放租约并把该 case 记 INVALID（`test_e2e.py::NegativeRunnerTests` 以私有锁目录覆盖这些性质）。

#1536：原地（committed）模式的整个 case 循环持有本树 `.negative-source.lock` 树锁（与 memory 预演同一把，timeout=900，同线程可嵌套 mutating_sources 的内层树锁）。前提因此是**同树不得并发两个 runner**：正控在租约窗内读受管字节，若同树第二个 runner 不被树锁挡住，就能在 case 间空窗写入这些字节；并发尝试会被树锁串行化为后到者 900 秒获取超时、全 case INVALID（fail-closed），而 #1402 的 case 间空窗语义对异树保持不变。`test_negative_guarded_run_holds_tree_lock` 钉锁覆盖整轮，`guarded-run-without-tree-lock` 负控摘锁。#1528：共享 `mutating_sources` 的 parity 租约**获取预算**改为跟随调用方 `lock_timeout`（本 runner 用默认 900 s），与单 case 持有上界 1500 s（BUILD 900 + 2×BOUND 300）同量级——此前 60 s 获取上限会让排在异树冷 cargo 构建窗后面的 runner 提前 INVALID。异树并行仍需自行错峰：若需要等待对窗树超过 900 s，调用方传更大的 `lock_timeout`；等不到预算按 fail-closed 记 INVALID，不静默重试。#1529：受管路径清单 `paths` 必须显式含 `scripts/gateway-parity/session_domain.py` 等前缀成员（`test_negative_guarded_sources_take_parity_lease` 非 mock 守卫 + `omit-parity-guarded-source` 负控），否则真实租约会静默消失而 mock 类用例仍全绿；`NegativeRunnerTests` 的替身也因此用显式 `__enter__`/`__exit__` 类而非 `@contextlib.contextmanager`（后者在 GC 关闭生成器时会伪造提前 release 事件，`lease-enter-without-release` 的杀须依赖真实缺 __exit__ 而非 GC 时机）。

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py --check-anchors
# 提交后，调用方已提供 CARGO_TARGET_DIR：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py <新证据目录> [--case NAME]
# 未提交 Python 源码的只读内存预演，同样要求正控/具名断言/恢复正控：
python3 -I src/pilotdeck-gateway/tests/fixtures/native-session-e2e/negative_controls.py <新证据目录> --memory --case drop-contract-connections
```

Rust 入口 `native_session_e2e.rs` 拉起 Python 套件。正式冻结 CLI 的 HEAD 与登记核验仍由提交后的协调者链执行，内存加载与合成证据不能替代它。

fix-r7：provider 预留 socket 与 HTTPServer 同用 SO_REUSEADDR，避免正常 HTTP 关闭后的 TIME_WAIT 被误认为活服务；gateway 仍按无复用策略预留。失败的保留尝试关闭两只 socket。Gateway.close 主动关闭 listener/活动连接并有界 join；这里都是本进程线程，没有 provider/gateway 子进程。class cleanup 在 setUpClass 成功前登记，异常也清理临时目录。`LifecycleTests` 覆盖同端口对连续两格、预留失败、未连接/半帧取消、采集异常；新配对变异归 `native-witness/negative_authentication.py`，其 `--memory --case provider-probe-no-reuse` 可在未提交树预演，完整源变异仍留提交后的宿主执行。

fix-r8：close 的 True 同时要求全部 manifest case 已发完最终帧、线程已退出、没有错误；未完成取消即使资源已回收仍返回 False。连接空闲读保留 180 秒，取消靠 shutdown 唤醒而非等待超时。profile 两条负控由 LifecycleTests 显式调用采集 harness，仅将精确的 session profile 异常转成测试专属 AssertionError，不把 setUpClass ERROR 或无关异常算作 KILLED。macOS 生命周期和 SO_REUSEADDR 负控见 fix-r8-res/RESULT.md；Linux 复核待协调者宿主执行。

fix-r11：全期望集保留“不适用 registry”等投影输入，默认直接读这些合成原件；实际落盘等待另由 `native-witness/test_r11.py::R11Tests.test_collector_wait_wired` 以 `real_settle=True` 走完整采集器。mock 的 `after_final` 钩子在 final 发出后暂停写盘，测试读到 running 原件后才释放，验证先读不完整、新实现等待完整的确定性交错。

#1510：端口对除两只预留 socket 外，gateway 与下一 provider 端口各持一把 flock 租约，覆盖「预留 socket 释放 → 真实 bind」之间只剩租约设防的交接窗口（窗口内第三方仍可 bind，所以 `test_port_reservation_is_exclusive_across_processes` 让子进程在窗口内重扫，必须选到别的对）。租约文件固定在 `/tmp/pdrs-e2e-port-leases-<uid>/pdrs-e2e-port-<port>.lock`：目录 0700 且属主必须是本 uid，显式**不跟随 `$TMPDIR`**（否则两个 TMPDIR 不同的副本会给同一端口锁不同文件，交接窗口里互不排斥）；文件用 `O_NOFOLLOW` 打开、**从不删除**（unlink 会让同路径换 inode，两边各锁一个文件），进程退出由内核释放锁，崩溃也留不下陈旧租约。取到锁后刷新一次文件时间戳：外部的 `/tmp` 清理器（Linux systemd-tmpfiles 默认按 10 天）可能把仍被持有的租约文件当陈旧条目 unlink，之后下一个 opener 会锁到新 inode，交接窗口重新打开。只有 flock 的 `EACCES/EAGAIN/EWOULDBLOCK` 记「该端口被占」并继续扫描；打开租约文件本身的失败（`EACCES/EPERM/ELOOP`）与其他 errno（不支持的 FS、EMFILE…）一样按环境故障抛出，不伪装成端口耗尽——0700 且属主为本 uid 的目录里出现打不开的租约路径，说明租约树已被破坏，把这种情形记成「被占」会让一次被拒的扫描看起来像证明了互斥；失败候选的两把租约在所有分支都关闭。一次扫描最多 8 遍、间隔 50 ms（`PORT_SCAN_PASSES`/`PORT_SCAN_PASS_SECONDS`），兄弟副本的瞬态占用靠预算跨过，真实耗尽仍以原因表失败。因此「别的副本正持这对端口」表现为本副本改扫下一对，跳过原因只写进 reason map，不静默变绿。边界：这套端口租约**不是** `scripts/gateway-parity/common.py` 的全仓 parity 租约（`/tmp/pdrs-parity.lock`）；夹具在宿主链里通常已在后者之内运行，再取一次会形成两种租约嵌套、放大死锁面，跨仓全量对拍与本套件的互斥仍由调用方持有的 parity 租约负责。
