# #745 SessionFactory 生命周期金样与负控

`oracle.json` 记录固定 TS commit / 源码闭包 SHA / Node v22.22.0、20 格生命周期矩阵、13 个完整投影探针和 3 个默认 storage hydrate 错误。原始 20 格探针来自协调者 `_tmp/20261028-issue-745-matrix/`；`probe.mts` 保留其操作和确定性门闩，基线/输出路径为参数；fix-r1 增加两个 writer 交错、仅内存状态覆盖探针。`normalize.mts` 只归一登记的临时路径、运行目录和耗时，`check.py` 先验证物理证据再归一。

`projections.mts` 调真实 AgentTranscriptProjections、AgentLoop.snapshotFileState、SessionRuntime / 默认 SessionProjectionDriver。原矩阵的 readFiles/writtenFiles 是测试注入的非生产形状；现已改成无 seed 时返回 `{}` 的 stdio 语义。`file-state.mts` 用真实 TS stdio、工具 checkpoint 与用户可见终态实测 initial/running/terminal/after，非空 SeedState 的字段与保留另由 Rust 用例对照。

以下命令从本树根运行；所有 cargo 使用调用方的同一 target，不建副本仓库或第二份构建目录。

```sh
export CARGO_TARGET_DIR=/Users/da/ws/pilotdeck-rs/_build/wt-1122/_build/cargo-target
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
scripts/cargo-slots.sh
cargo test --offline --manifest-path src/Cargo.toml -p pilotdeck-gateway --lib runtime::session::factory::tests -- --include-ignored --nocapture
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py --audit
```

164 项变异绑定完整 Rust 测试名和专属失败消息。先应用补丁并提交，使所有变异目标等于 HEAD；再由协调者在宿主运行长负控，OUT 必须为尚不存在的目录：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py --compile "$OUT_COMPILE"
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py "$OUT_NEGATIVE"
```

可在 OUT 后加单个变异名（如 `same-key-cached`）。每项先跑正控、单独编译变异、运行精确被点名测试、还原并复跑；所有项目汇总后退出。编译失败、错断言、槽位/测试超时、源码不等于 HEAD 或恢复守卫失败均为 INVALID；任何非 KILLED 使全量失败。`--compile` 的 COMPILED 仅表示能编译，不表示被杀。等待槽位与单次进程均最多 900 秒；Rust 门闩最多 8 秒。执行器进程/槽位超时不作为判杀证据；本单取消候选的两条源码变异只以 `#1122 pending candidate explicit disposal reason` 等值断言判杀；观察、构造和清理的截止均不匹配该标记，不能当作 KILLED。

状态差异正式登记在 `specs/gateway-parity/diffs/A7.2-SESSION-{WRITER-FENCE,SAME-KEY,RECREATE-DRAIN,ADMISSION-READ}-001.md`。同 key 并发、重建排队与 admission 首读错误清理仍标未批准；writer fence 依「基线丢数据默认修」人定规则实施、待人复核，不自动增加评分豁免。

`writerInterleavings` 保留 TS 的两时序损坏，显式标 `tsDefect:true`。Rust 的两个同名交错测试改为严格递增、旧 recorder 关闭后不得写、新 handle 继续 append、后续 resume 成功。Native 本包无完整 host transport，测试的尾写由真实 recorder 重放；不宣称完整模型 turn 验收。

`fenceMatrix` 新增 4 个旧状态（idle/quick/never/disposing）×4 事件（recreate/twice/close/shutdown）×3 时机（beforeload/afterload/publish）共48格。publish 门闩在真实 Router setupSession、registry.replace 之前；never 表示测量前缀中未放行的旧 turn，探针仅在清理时解除，不把15秒装置截止当产品结论。Rust每格有完整 `fence_<state>_<event>_<timing>` 测试；never 的后两阶段被 fence 阻断于 load 前，以专用超时、零load/零publish与原文件验证。

租约按 canonical path 跨 factory 协调，并复用既有 `.lock` 文件锁排除其他进程。锁随真实 recorder/store 保留；显式 close 等已受理写完成后释放，不依赖旧 handle clone Drop。recreate 前驱 drain 与忙锁等待预算由 `FactoryOptions.writer_busy_wait_timeout` 显式传入；宿主按实际取消策略选择（例如既有 `POST_CANCEL_EXIT_TIMEOUT`），不隐式借用默认 transport close_timeout，storage构造自身耗时不消耗租约等待预算；超时不取消旧 handle 的持有型 drain任务。

`regenerates_identically` 必须执行真实探针，生成器不再接受 `MATRIX_IMPORT_ROOT` 跳过采集。新增金样仍由同一个 `#[ignore]` 守卫逐字节重生成验证。`--audit` 只证明变异文本发生变化、测试存在、失败消息存在；语义非等价与判杀只能由编译和实际测试证明。

fix-r2 的文件观察在写入期间只排除已知 checkpoint 路径、当前 PID、UUID v4 的原子发布暂存文件。其他 `.tmp`、持久文件读取错误仍原样报错；`after-owned-cleanup` 和 `finalFiles` 保留全部文件。`file-snapshot-test.mts` 调真实 TS checkpoint store，在 `readdir` 后、`lstat` 前完成真实 rename，验证暂存路径消失、新内容已发布、最终证据和错误传播。该验证由生成器在独立进程执行，整个进程（包括清理）硬截止15秒。

TS `getOrCreateActive` 在等待旧 dispose 后读取共享 `cached.handle`，所以第二次重建可能返回先发布的 handle，也可能返回随后覆盖它的 handle。主48格的12个 `twice` 格明确采用 **旧 dispose 返回晚于最后候选发布** 的确定性时序；只延迟真实 storage dispose 完成后的返回，不延迟旧 turn 的 abort。`twiceReturnOrders` 另保留 idle/twice/beforeload 的 early 与 late 两种实际结果（第二请求分别返回2、3），没有把真实早返回归一化掉。48个 Rust 用例检查 TS 完整字段；Rust 的旧状态与首调用结果直接对照，writer fence、串行重建/close 的差异分别按既有登记断言，未批准部分仍未批准。代表格还断言 Rust 身份关系落在两种 TS 观测集合中。

`matrix-diagnostic-severity` 改注入 `warning`：malformed 格的真实 TS 值是 `error`，原 `error→error` 是等价变异。fix-r2 的91项中，新增21项覆盖文件观察和完整矩阵/返回集合；未提交的新文件不得绕过 `mutating_sources`，须提交后在宿主逐项 `--compile` 再完整执行。

`closeReturnOrders` 同样保留 idle/close/beforeload 的两种真实错误：旧关闭先完成时为 `Agent is not registered`，候选先碰到已移除的 scoped registration 时为 `Scoped service is not registered`。主48格的12个 close 格用 storage-dispose 返回门闩固定后一时序，第一候选拒绝后释放；其他事件不复用这个释放条件。shutdown 必须先等待创建完成，不能加入此门闩。

fix-r3 增加真实 TS `prepareDisposal`：第二次 prepare 的 plugin.refresh 钩子先完成旧 session dispose，随后 factory snapshot 拒绝。Rust 同 owner 门闩验证本次相同错误、prepared资源释放、下一次从磁盘resume；不再保留prepare前快照。

48格现在在 Rust 实际 boundary / event / result 构造同形对象，以原始 oracle 行为底稿，只映射 WRITER-FENCE-001-R1 与 RECREATE-DRAIN-001-R1 明列字段。oldInFlight读取真实 handle.activity；旧turn用activity guard建模，fulfilled由活动归还及drain完成验证，非模型turn返回值。first/second 的完整错误和 handle 身份则直接读取真实 factory/owner 返回。registryAfterEvent也读取owner实际缓存；shutdown提前移除与同key排队仍是未批准差异。

writer lease在dispose/rollback一开始被take到局部WriterCloseGuard；guard在正常退出、panic或JoinError路径先同步截断新写，把writer和租约移交独立blocking线程，已受理写完成后才drop租约；runtime不会被commit锁阻塞，runtime已退出仍可清理。release/close钩子的最后写入仍按原顺序保留。panic释放、旧writer退休、下一次create、非默认fence预算与已open_locked store拒绝均有回归和专属变异。FactoryStorage::new必须接未锁store；#1019起已锁recording经`SessionStorage::Recording`/`open_and_adopt`把fence转移给factory租约（编译期开关负控见`session-adopt/`），不得通过跳过锁绕开排他性。

fix-r4 将 handle 的 writer guard 接管提前到 adapter.abort 之前，覆盖 abort panic 尚未调用 session.dispose 的路径；正常资源钩子的最后写入仍在 guard Drop 前。factory.new 明确拒绝 ZERO 预算；到期仍完成 canonical path 和一次非阻塞租约尝试，只有忙锁等待受 deadline 约束（忙锁可等待时间仍扣除路径解析耗时）。耗尽预算用真实前驱活动、受控暂停 factory future、虚拟时钟与实际 lease 释放验证，不用 sleep 猜时序。

原 #745 fix-r4 新增五项：`writer-abort-panic-cleanup`、`writer-zero-budget`、`writer-expired-budget-skips-try`、`same-key-concurrent-skip-cache`、`same-key-cached-skip-cache`。same-key 新变异直接跳过缓存返回；原两条只覆盖 dirty，证据范围按 `A7.2-SESSION-SAME-KEY-001-R1` 更正。未提交产品源码的 mutating_sources 拒跑必须保留；本地一条样本不代表全部150项已编译/判杀。

#1022 补全 ticket 排队负控、FIFO/取消/截止、慢路径预算与 drain seed。`writer_busy_wait_timeout` 不限制 prepare、storage construction、canonicalization 或单次 OS 文件打开/锁尝试的墙钟时间；路径时间仍消耗后续忙锁剩余预算。`followups.mts` 用真实 stdio 实测 drain checkpoint 与 abort throw 后清理，`followups_regenerates_identically` 重新采集并逐字节比较。Rust 的 abort panic 被捕获后仍等 activity 清空再调用 session.dispose，保留原错误；测试保留旧 handle 验证资源无需等 clone Drop。

优先解除 #748b 负控前置：提交后运行 `negative_controls.py OUT same-key-ticket-bypassed`，必须编译成功且 KILLED；本树未提交时 HEAD 守卫拒绝是正确行为。新增负控完整清单与执行状态见任务证据 RESULT，不把静态锚点检查称为运行判杀。

#1022 fix-r1：原 `same-key-ticket-bypassed` 在 fdeea180d 实跑 KILLED；两个 worker 都跳过 enter，第二个不会等待前驱信号，不修改这条有效变异。abort 的 &str/String panic 原文与 TS `ABORT_1022` 比对，非字符串 payload 沿用 fallback。已成功取得重建快照的前驱保留 source 到 owner 重试，首次 dispose 错误仍返回；只有已 Disposed 且 activity 已归还时才跳过旧错误，外部提前 dispose 不授予资格。追加差异范围见 `A7.2-SESSION-DRAIN-FILE-STATE-001-R1.md`。

新增 dispose-throw 真实 stdio 金样证明 drain 后 checkpoint 与资源清理，不宣称整个 TS factory/retry 端到端一致；`dispose_failure_followups_regenerates_identically` 重跑完整生成器。新变异与改写的旧锚点均须提交后宿主逐项编译、执行；当前未提交源码触发 HEAD 守卫是 INVALID，绝不计入 COMPILED/KILLED。

#1022 fix-r2 新增 13 项 `r2-*` 变异；fix-r3 再加两项，总数 150。`r3-owner-replays-drained-busy` 与 `r3-idle-sweeps-pending-error` 使用 `#[cfg(test)]` 内的 `option_env!` 编译期开关，分别绑定取消后 drain 完成的重试与 idle sweep 保留未消费清理错误。两项不写产品源码；执行器持有同一树锁、逐项 no-run、精确测试、清除开关后重跑正控。原有 148 项仍必须满足原地变异的 HEAD 守卫，开关模式不豁免任何源码变异。仅编译两项可用 `--compile OUT r3-owner-replays-drained-busy r3-idle-sweeps-pending-error`；完整样本去掉 `--compile` 并只选一个名字。所有 cargo 均复用调用方 target，最终全族由协调者在宿主执行。

pending 仅在错误码为 `SESSION_WRITER_BUSY` 且前驱 `recreation_drained()` 后失效；dispose 清理错误继续保留到外层 caller 消费。idle sweep 不移除 pending record。真实 TS AgentHandle 的 disposal/retry 四格复用 `session-disposal-retry` 金样；Rust future 取消、writer budget、pending 跨 sweep 保留不宣称 TS Router 端到端一致，范围补充见 `A7.2-SESSION-DRAIN-FILE-STATE-001-R3.md`。`writer_busy_wait_timeout` 的墙钟排除项也包括 initial recreation snapshot。

#1020 owner 已选 previous 后同 key dispose 完成的窗口由受限重试覆盖：factory 在 prepare 前捕获 `Disposed && !retains_recreation_source()`，post-prepare 快照拒绝时打 `recreation_stale_handoff` 标记并原样返回 TS 错误；owner 重跑一次同步选择，死 handle 不再入选。直接 factory 调用方、prepare 中 dispose、重试上限 1 与初始 Disposed 过滤各有正控；TS Router 无过滤无重试（四格新金样 `session-stale-handoff/`，`stale_handoff_regenerates_identically` 守卫），差异登记在 `A7.2-SESSION-DISPOSAL-RETRY-001.md`。新增 4 项负控（总数 154）：`1020-stale-mark-removed`、`1020-stale-retry-removed`、`1020-stale-retry-unlimited` 用 `#[cfg(test)] option_env!` 编译期开关（`PDRS_1020_*`），`1020-stale-fixture-regeneration` 防金样漂移。

#1020 fix-r1 按评审再增 4 项（总数 158）：`1020-generator-missing-out`（生成器提前 mkdir 被删，README 直跑命令的 OUT 原先不存在 → ENOENT，守卫改传不存在的 `out/` 子目录以覆盖该入口）、`1020-generator-work-dirs-kept`（清理变回字面量 `work-`，4 个 `work-*` 残留 → 守卫断言 OUT 内除 oracle.json 外无残留）、`1020-setup-passes-stale-previous`（owner 把已 Disposed 的缓存 handle 递给 setup → `recreate_retries_stale_disposed_handoff` 的 setup(previous) 契约断言失败）、`1020-second-error-flattened`（owner 上抛错误被拍平丢 code/causes → `stale_handoff_retry_is_limited_to_one` 断言第二次错误的完整形状）。

#1020 fix-r2 按评审再增 3 项（总数 161）：`1020-generator-deletes-unrelated-out`（生成器清退变回按 `work-` 前缀扫描 OUT → 删掉既存无关内容，新守卫 `stale_handoff_preserves_unrelated_out_files` 逐字节比对失败；生成器改为只删本次 `mkdtempSync` 自建根，`1020-generator-work-dirs-kept` 锚点随清理行同步改写但语义不变）；`1020-publication-before-setup` 与 `1020-publication-before-setup-completed`（owner 把 `DirtyRecreate` 的 on_evict 发布移到 setup 之前，恰一次 → 两家顺序断言 `ordered("setup.fresh.2","evict.dirty-recreate")` 分别失败，setup 事件自 fix-r2 起带尝试序号）。


#1122 fix-r2：`drain-cancelled-candidate-not-disposed` 保留原源码变异，但改绑 `#1122 pending candidate explicit disposal reason`。原测试只检查释放，#1010 的最后句柄 Drop 会以 `agent_handle_dropped` 清理，因而能掩盖显式 dispose 缺失。现在同一被点名测试保留真实取消重建格，再用真实工厂候选 + `PendingSession` + maintenance guard 检查显式 reason。guard/观察句柄阻止最后句柄 Drop；current-thread runtime 直接 block_on，直到 reason 出现或本候选清理 future 的完成通道关闭。观察以 1ms sleep 让出 executor，不依赖全局任务数或固定 yield 次数。观察结果先保存、清理后断言；8 秒装置截止的消息不与判杀消息匹配。新增错误 reason 变异与新金样重生成变异。

`cancelledDisposal` 由 Node v22.22.0 的真实 TS Router/Factory/AgentHandle 采集：加载暂停期间 shutdown 的候选 reason 为 `session_router_shutdown`，正常重建候选仍 active、没有 dispose reason；显式传入 `session_create_cancelled` 时 maintenance signal 与 listener 保留此 reason，并等待 held callback 归还后完成 dispose。TS Promise 没有 Rust future 的取消操作，**不声称 TS 自动使用 `session_create_cancelled`**。Rust 新子格只验证 PendingSession 交接与显式 reason，不声称真实未发布候选已经开始 maintenance。`cancelled_disposal_regenerates_identically` 为新增 ignored 守卫；生成器输出仍 recursive mkdir + 独占 wx 写入。

fix-r3 仅给 `PendingSession` 添加 `cfg(test)` 完成通知，非测试构建的产品行为不变。源码负控的共享守卫检查整个 gateway `src/` 是否等于 HEAD，因此修改 `factory_tests.rs` 后也必须先由协调者提交，再在宿主运行下列三格（沿用上方环境变量、同一 target）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py --compile "$OUT_COMPILE" drain-cancelled-candidate-not-disposed drain-cancelled-candidate-wrong-reason cancelled-disposal-regeneration
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py "$OUT_NEGATIVE" drain-cancelled-candidate-not-disposed drain-cancelled-candidate-wrong-reason cancelled-disposal-regeneration
```

#1122 fix-r3：观察 helper 的 8 秒截止必须以 `#1122 candidate observation executor settles` 失败；reason 永不出现且清理前驱永不完成的单测，用 20ms 截止捕获该精确 panic，并检查墙钟上界。另一单测覆盖 producer 完成/取消，保持一个无关 pending task 活跃，证明候选观察不依赖整个 runtime 静默。历史三格绑定不变；更正登记见 `A7.2-SESSION-DRAIN-FILE-STATE-001-R4.md`（未批）。

新增 `r3-candidate-observation-deadline-accepted` 编译期开关负控（`PDRS_1122_ACCEPT_OBSERVATION_DEADLINE`，仅在测试 helper 中生效）：把观察超时错误当 None 返回，必须由 `runtime::session::factory::tests::pending_candidate_observer_missing_reason_fails_by_deadline` 在 `#1122 missing reason must fail by observation deadline` 失败。它复用现有执行器的 tree_lock/no-run/正控/负控/恢复正控流程，不原地改源码，也不放宽源码变异的 HEAD 守卫。单样本命令：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/session-factory/negative_controls.py "$OUT_DEADLINE" r3-candidate-observation-deadline-accepted
```
