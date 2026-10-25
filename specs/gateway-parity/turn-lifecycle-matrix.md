# 轮生命周期矩阵：steer 端口 × 交互事件端口消费（#708）

补齐 #688（#586 第四包 + #589 交互端口按真实租约）引入的 steer 端口与交互事件端口消费的「状态 × 事件 × 时机」矩阵。本单只加测试与文档，不改产品源码；hooks 端口矩阵见 #693，permission 端口不在本单事件清单。

## 基线与证据

- TS 基线：`OpenBMB/PilotDeck@cc7c4e6`，探针 `src/pilotdeck-gateway/tests/fixtures/turn-lifecycle-matrix/generate.mts` 直接 import 未改写源码（`baseline_source_guard` 核验 blob），确定性 Promise 闸门、无 sleep；输出 `oracle.json`，两次生成逐字节一致（证据 `_tmp/20261025-issue-708/oracle-run{1,2}.json`）。复算命令见 fixture README。
- Rust 基线：origin/main c9d331c2（含 #688）。
- 已有用例锚点均为本单分支实测行号；`tests.rs` 指 `src/pilotdeck-gateway/src/runtime/project/interaction_steer/tests.rs`，`live_stream_tests.rs` 指 `src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs`，`interaction_host_tests.rs` 指 `src/pilotdeck-gateway/src/runtime/project/interaction_host_tests.rs`，`factory_tests.rs` 指 `src/pilotdeck-gateway/src/runtime/project/factory_tests.rs`。

## 轴定义

状态（对当前 session key 的端口绑定而言）：

- **S1 运行中**：run open（closed=false、external 未取消），mailbox 已 start。
- **S2 取消中**：external signal 已取消，monitor close 未完成（或在竞争 State 锁）。
- **S3 清理中·屏障前**：旧 run close 已发起但 close+finish 未排空（admitted durable write/discard 仍挂；steer 的 draining 屏障未过；events 的 Forwarding 链上有未完成预留）。
- **S4 清理中·屏障后**：前驱 close+finish 已成功 / 前驱预留已交付，但 dispose / 流收尾仍在进行。
- **S5 已清理**：run closed+finished、session/resource disposed、generation closed。

事件（题目给定）：E1 同 session 重提交；E2 steer（入队/条目取消/claim→durable→ack/drain）；E3 交互事件到达与预留；E4 预留释放（首/中/尾）；E5 消费方丢流；E6 取消。

时机轴并入状态（S3=清理屏障前、S4=清理屏障后）与前驱边界（steer：admitted durable write 完成前/后，见 S2×E4 两半；events：前驱 wire 预留交付前/后，见 S3/S4）。终止条件已覆盖：前驱永不完成（失败屏障 `tests.rs:403`、`:518`，dispose 超时 `interaction_host_tests.rs:588`）、前驱被取消（`tests.rs:329` cancel 等待写完、`:71` monitor 竞争）。

标记：**已实测对齐**（TS 输出路径 + Rust 用例）/ **差异已登记**（diffs ID）/ **待开单** / **不适用**（理由）。

## 表 A：steer 端口

### S1 运行中

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 重提交 | 差异已登记 A6.9-STEER-RUN-001（TS 无 session 级在跑闸门；同 ID finish+start 竞争下旧 insert 被接受且泄漏进新轮）。Rust 拒绝并保留旧轮权威为本单新增用例 | TS `oracle.json A3_barrier_race`（accepted:true, leaked:["a"]）、`A4_sequential.between`（轮间 no_active_turn）；Rust `tests.rs:568`（负控 `_tmp/20261025-issue-708/nc1-red.txt`：删 start_turn 的 session.run 存活检查即红） |
| E2 steer | 已实测对齐 | TS `A1_running_mailbox`（insert accepted、重复幂等、turn_mismatch、cancel pending→cancelled、tombstone→cancelled、offered→too_late、close 后 turn_closing、finish 后 no_active_turn）；Rust `tests.rs:194`（真实 TS trace 对拍）+ `tests/fixtures/steer`（`--test steer`，mailbox 序列/交错对拍） |
| E3/E4 预留 | 不适用（steer 端口无 wire 预留链；对应概念为 serialize 变更队列与 admitted durable write，并入 E2 与 S2×E4 前驱边界） | — |
| E5 消费方丢流 | 已实测对齐 | TS `A6_consumer_break`（消费方 break 后 TurnRunner finally 仍 close，discard a/b 落 recorder，事件不再被观察）；Rust 丢 `applying` future 仍完成 grant（`tests.rs:329`）、丢 close future 清理仍完成（`tests.rs:254`）、drop InteractionTurn close 恰一次（`interaction_host_tests.rs:329`） |
| E6 取消 | 转 S2 行 | — |

### S2 取消中

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 重提交 | 差异已登记 A6.9-STEER-RUN-001（同 S1×E1；取消未清理时同样拒绝） | Rust `tests.rs:568` 第二段 |
| E2 steer | 已实测对齐（abort 后拒绝；错误形式 HostError vs reason 属 A6.9-STEER-RUN-001 已登记范围） | TS `A2_abort_gate`（abort 后 session.steer → no_active_turn，错误 turnId → turn_mismatch）；Rust `tests.rs:71`、`:329`（取消后 enqueue 拒绝） |
| E4 前半（durable 前） | 待开单（差异已登记 A6.9-STEER-RUN-002，本单新增） | TS `A2_apply_after_abort`（abort 注入 drain 与 apply 之间仍完成 claim→durable→ack 并 yield steer_applied，modelCalls=0）；Rust pre-durable check 拒绝、无 claim/授权/append，条目随后 `turn_ended` discard（`tests.rs:656`，负控 `nc2-red.txt`：删 `TurnSteer::check` 的 external 取消子句即红） |
| E4 后半（applying 已入锁 / durable 成功边界后） | 已实测对齐 | TS `tests/fixtures/interaction-steer/cancel-oracle.json`（durableThenAbort：成功边界后 ack/grant 照常）；Rust `tests.rs:329`（成功边界仍 grant/ack，close 等写完，cancel 等待 writer） |
| E5 drain | 已实测对齐 | TS cancel-oracle（abortDrain：取消后先 drain 后 close 均按 mailbox 实际状态返回，不转 HostError）；Rust `tests.rs:105` 对拍同一 oracle |
| E6 取消 monitor | 差异已登记 A6.9-STEER-RUN-001（Rust 取消 monitor 与 mutation admission 共用 State 锁，check 与入队临界区之间 close 不得越过；TS finally 串行无此竞争） | Rust `tests.rs:71`（`closed != blocked` 判据为 A6.9 机器判据的一部分） |

### S3 清理中·屏障前（前驱 close+finish 未排空 / admitted write 挂起）

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 重提交 | 差异已登记 A6.9-STEER-RUN-001（屏障前 start_turn 拒绝或等待；wait_ready 等全部前驱；失败屏障只堵同 key，其他 key 可用） | Rust `tests.rs:254`（insert 挂起中拒绝+等待）、`:448`（未启动绑定的连续替换不移除屏障）、`:518`（失败屏障同 key 重复 rebind 均拒）、`interaction_host_tests.rs:588`（dispose 超时重提交等旧 recorder） |
| E2 steer | 已实测对齐 | TS `oracle.json A1_running_mailbox.trace`：trace[7] `{"messages":[],"closed":true}`（drainOrClose 已关）之后、trace[9] `finished` 清空之前，trace[8] `{"accepted":false,"reason":"turn_closing"}`（oracle.json:45，与 trace[10] `no_active_turn` 相区分）——close 已发起、finish 未排空时 enqueue 拒绝。+ `A4_sequential`（未 drain 条目 close 时 discard reason `turn_ended`，事件序 `steer_unapplied` 先于 `turn_completed`）；Rust `tests.rs:254`（insertion err + 恰 1 条 turn_closing discard） |
| E4 前驱 durable 保护 | 差异已登记 A6.9-STEER-RUN-001（applying 锁保护 admitted durable write，close 等待 applying；TS 对应竞争即 A3 泄漏） | Rust `tests.rs:329`、`:448`、`interaction_host_tests.rs:588` |
| E6 | 并入 E1/E4 行 | — |

### S4 清理中·屏障后（前驱 close+finish 成功 / 前驱已交付）

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 重提交 | 已实测对齐（屏障过后新轮正常启动；拒绝闸门本身属 A6.9-STEER-RUN-001） | TS `A4_sequential`（turn2 干净启动：drained 仅 c、事件序完整）；Rust `tests.rs:568` 尾段（close 后新 turn enqueue 成功、旧句柄拒绝）、`:403`（grants 不跨 run） |
| E2 steer（旧句柄） | 已实测对齐 | TS `A4_sequential`（旧 turnId → turn_mismatch）；Rust `tests.rs:254`（旧 run/session/generation 身份拒绝）、`factory_tests.rs:3963`（旧 generation lease drain 前保持但不可达新代） |
| E4 成功边界 | 已实测对齐（同 S2×E4 后半：durable 成功后取消/dispose 不撤销 grant/ack） | Rust `tests.rs:329` |

### S5 已清理

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 资源已关 bind | 不适用（TS 无资源关闭 API；Rust 加固属 A6.9-STEER-RUN-001 范围） | Rust `tests.rs:518`、`factory_tests.rs:3871`（revoke 精确句柄/错误身份拒绝） |
| E2 旧句柄 enqueue/cancel | 已实测对齐 | TS `A1_running_mailbox`（finish 后 no_active_turn）、`A4_sequential`（旧 turnId → turn_mismatch）；Rust `tests.rs:254`（check_identity）、`:518` |
| E5 旧 router drain | 已实测对齐 | TS `A1_running_mailbox`（closed 后 drainOrClose 空、enqueue turn_closing）；Rust `tests.rs:254`（drain 身份拒绝）、`--test steer` |
| E6 close 幂等/释放恰一次/失败可观察 | 已实测对齐（清理失败保持失败、runtime 关停可观察为 Rust 加固，A6.9-STEER-RUN-001） | TS `A1_running_mailbox`（drainOrClose 关闭语义、finish 清空）；Rust `tests.rs:194`（close×2、release_count=1、discarded 只交付一次）、`:144`（runtime 关停）、`:179`（poison 可观察）、`:403`（失败可重复观察、禁止复用） |

## 表 B：交互事件端口

wire 预留机制（validate_event 预留槽、EventGuard 释放、Delivery 前驱拼接、final 预留 tail）为 Rust 实现（`runtime/turn/live.rs` Forwarding 链），TS 只有单一 generator pump（`agentLoopSidecarClient.ts:379-399`）无对应机制——E3/E4 各格按「差异已登记 A7.2-INTERACTION-CONSUME-001(+R1/R2)」处理，对拍只断言可观察交付顺序。

### S1 运行中

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 并发活动轮 / bind_turn 顶替 | 差异已登记 A6.9-INTERACTION-EVENTS-001-R1（已批；Rust 后次 bind_turn 撤销先前句柄、adapter 拒并发活动轮、dispose 后拒；TS createAgentEventBuffer 无 close/拒绝 API） | TS `B2_event_buffer`（正常序：到达序保留、drain 清空、再 drain 空、晚到单独成批）；Rust `factory_tests.rs:3780`（真实 TS 金样重放+两种 builder 顺序）、`live_stream_tests.rs:1442`（顺序轮 fresh hosts）、`:1494`（非法 submit 不消费 session）、`:1503`（未 poll drop/工厂失败释放活动轮） |
| E3 到达与预留 | 差异已登记 A7.2-INTERACTION-CONSUME-001（wire 接收预留交付顺序，host events 不越过未应用的 wire lifecycle） | Rust `live_stream_tests.rs:1745`（buffered 批等待预留顺序）、`:1702`（turn_started 前缓冲 fail-closed）、`:1948`（对象 payload 应用并转发）；TS 侧可观察顺序对拍 `B2_event_buffer` + `factory_tests.rs:3780` |
| E4·首（消费预留/交付） | 差异已登记 A7.2-INTERACTION-CONSUME-001 | Rust `live_stream_tests.rs:1745` |
| E4·中（EventGuard 释放/前驱拼接） | 差异已登记 A7.2-INTERACTION-CONSUME-001-R2（校验后弃队路径释放预留槽；guard 保留到应用结束） | Rust `live_stream_tests.rs:2038`、`:2083`、`:2118`（中段弃保序）、`:1970`（abort 释放已 validate 预留）、`:2167`（guard 只释放自己的槽） |
| E4·尾（final 预留 tail / 链拆） | 差异已登记 A7.2-INTERACTION-CONSUME-001 / -R2（嵌入回调 final 分流真值语义） | Rust `live_stream_tests.rs:2153`（delivery 链无 runtime 不递归）、`:2198`（嵌入 final JS 真值）、`:1804`（host 错误截断已预验交付） |
| E5 消费方丢流 | 不适用（TS 唯一 pump 无消费方丢流场景；Rust LiveTurnStream 通道行为属 #589 登记范围：receiver 关闭 → "LiveTurn event receiver closed."，丢流释放活动槽并 dispose handler） | Rust `live_stream_tests.rs:360`（closed channel 等执行清理 + Aborted 结果）、`:1385`（drop 活动流 dispose handler） |
| E6 取消（cancel exit 终态 fail-closed） | 差异已登记 A7.2-INTERACTION-CONSUME-001（StreamIntegrity 完整性检查为 Rust 加固） | Rust `live_stream_tests.rs:725`-`:1148` 系列（cancel exit 丢队列应用、violation 后 settle None、缺 input_accepted 拒绝）、`:396`、`:1097`、`:1123` |

### S2 取消中

| 事件 | 判定 | 证据 |
|---|---|---|
| E3/E4 取消中释放已 validate 预留 | 差异已登记 A7.2-INTERACTION-CONSUME-001-R2 | Rust `live_stream_tests.rs:1970`（aborted 执行释放预留） |
| E5 closed channel 清理 | 差异已登记 A7.2-INTERACTION-CONSUME-001 | Rust `live_stream_tests.rs:360` |

### S3 清理中·屏障前（前驱预留未完成）

| 事件 | 判定 | 证据 |
|---|---|---|
| E3 到达事件不得越过未完成前驱 | 差异已登记 A7.2-INTERACTION-CONSUME-001（单链保序） | Rust `live_stream_tests.rs:1745`、`:2083`（pending module 顺序） |
| E5 丢流时 pending callback 不搁浅 | 差异已登记 A7.2-INTERACTION-CONSUME-001-R2（Delivery 拼接前驱） | Rust `live_stream_tests.rs:1970`、`:1385` |

### S4 清理中·屏障后（前驱预留已交付 / generation dispose 中）

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 丢流后立即重提交同 session | 差异已登记 A6.9-INTERACTION-EVENTS-001（旧 dispose 并行不打扰新轮，lease 各自持有；TS 无并行 dispose 场景，顺序行为见 A4） | Rust `interaction_host_tests.rs:475`（drop 后立即重提交）、`:588`（dispose 超时重提交等旧 recorder）、`factory_tests.rs:3963`（registry 保旧代直到 lease drain） |
| E3 generation closed 后 emit/drain 拒 | 差异已登记 A6.9-INTERACTION-EVENTS-001（显式拒绝，不把空列表当成功） | Rust `factory_tests.rs:3871` |
| E3 inner 已 dispose 后到达回调 | 差异已登记 A7.2-INTERACTION-CONSUME-001（drain 失败截断为 host_error） | Rust `live_stream_tests.rs:1804`、`:1622`（buffer 失败截断后续事件/终态） |

### S5 已清理

| 事件 | 判定 | 证据 |
|---|---|---|
| E1 流结束后新轮 fresh hosts | 已实测对齐 | TS `A4_sequential`（turn2 事件序完整、旧 turnId turn_mismatch）；Rust `live_stream_tests.rs:1385`、`:1442` |
| E4 终态只投影一次、next→None | 已实测对齐 | TS `B5_unapplied_order`（成功/错误路径尾段事件序：steer_unapplied 先于 turn_completed；错误路径 turn_failed 先于 steer_unapplied）；Rust `live_stream_tests.rs:253`（payload 背压+终态只投影一次）、`:1288`（abort 终态投影一次）、`:1148`（violation 后 settle None） |

## 差异登记汇总

- A6.9-STEER-RUN-001：steer 端口加固（session/run 身份、清理屏障、取消 monitor、错误形式）。A-S1×E1、A-S2×E1、A-S2×E6、A-S3、A-S5×E1。
- **A6.9-STEER-RUN-002（本单新增，待开单）**：A-S2×E4 前半——TS 在 abort 后仍应用已 drain 的 steer（claim→durable→ack 完成、steer_applied 事件、modelCalls=0），Rust 在 durable 前拒绝并转 discard。方向为安全收紧（取消路径少应用一条消息/一项授权），是否对齐 TS 待维护者裁决。
- A6.9-INTERACTION-EVENTS-001(-R1，已批)：InteractionEvents/TurnEvents 资源生命周期加固。B-S1×E1、B-S4×E1/E3。
- A7.2-INTERACTION-CONSUME-001(+R1/R2)：wire 预留/顺序交付/终态完整性加固。B 全表 E3/E4/E5/E6 相关格。

## 本单新增用例与负控

- `tests.rs:568` `interaction_steer_start_turn_while_run_open_or_cancelled_is_rejected`（A-S1×E1/A-S2×E1）。负控 NC-1：删 `interaction_steer.rs` start_turn 的 session.run 存活检查 → 首个断言红（`_tmp/20261025-issue-708/nc1-red.txt`），逐字节还原（SHA256 一致）后绿（`nc1-green.txt`）。
- `tests.rs:656` `interaction_steer_persist_and_apply_after_cancel_rejects_and_discards`（A-S2×E4 前半；含 spawn-先于-cancel 与 cancel-先于-spawn 两序，拒绝侧经 `close_events` 的 `reject-cancelled`/`reject-closed` 测试 cfg 观测点断言——fix-r1。fix-r2 如实登记调度前提：**前半依赖 tokio current-thread 运行队列 FIFO**——先 spawn 的 applying 任务先于 cancel 唤醒的 monitor 被 poll——该前提下确定性已由协调者仲裁实测 80 次（全名逐次断言「1 passed」40/40、6 满载 CPU 干扰下 40/40，协调树 `_tmp/20261025-issue-708-gate/r2/arbitration.txt`）；前提变化时前半断言转红而非假绿。后半先等 `closed` 事件再 spawn，与调度顺序无关）。负控 NC-2：删 `TurnSteer::check` 的 `|| self.run.external.is_cancelled()` → 取消侧断言红（`_tmp/20261025-issue-708/fix-r1/nc2-red.txt`），逐字节还原后绿（`_tmp/20261025-issue-708/fix-r1/nc2-green.txt`）。
