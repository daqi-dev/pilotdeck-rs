---
type: experiment
number: "015"
date: 2026-10-15
title: A5.2b · 双连接、交错 close、存储故障与重启夹具
status: 第 2 轮生产 close 时序仲裁完成；真实 sidecar G 交叉确认完成（一格）；cold-restart 与真实 WS 已由协调者实跑（WS 跨遍汇总待补，#112）
related: [plan/005_phase-a-session-domain, experiments/exp014-a52-session-fixtures, research/008_phase-a-session-domain/gap]
---

# A5.2b 基线受控实测

**34 个子例 × k=5 = 170 次进程内测量，全部一致；4 个装置级、6 个观察副本对照均命中指定规则。真实 stdio sidecar 另做 5 次 G 路径交叉确认；原 A5.2a 装置原样回归 360 次及 9 个原有对照。** 首轮共 535 次测量、19 个对照，不含任何 Rust 对拍分数。cold-restart（2 组 × 5，结果一致）与真实 WS（15 子例 × 5 = 75 次执行完，跨遍汇总待补）已由协调者在沙箱外实跑，结果见下表；真实 Web bridge 重连仍单列为未验，不能据此关闭 D5。

**第 2 轮将旧 close 误中新 run 的结论降级为「机制存在、指定生产次序未达到；源码显示成功路径窗口只有微任务（单会话）」：范围限定为本轮成功清理、真实 FileRunRegistry 路径。** 去掉额外 WS return 等待、保持 disconnect 同步后，单会话两种次序各 5/5 未命中新 run；多会话两种次序各 5/5 也未命中。权限/问题的四组无 hold 次序另做 20 次，原 binding/abort 结果可复现。共新增 **8 组 × 5 = 40 次**，每次通过原三面检查器；另原样回归主夹具 170 次、10 个对照。第 2 轮报告与逐次证据哈希见 [production-close-results.json](production-close-results.json)，不合并为原 535 次的首次采集。

逐例摘要、完整集合、装置与证据哈希见 [results.json](results.json)。**正式采集**（预注册规则先单独提交（本 PR 第一个提交；rebase 合并会改哈希，以提交内容为准），记录为 COMMITTED_BEFORE_COLLECTION）：进程内 [`_tmp/a52b/coordinator-main/`](../../../_tmp/a52b/coordinator-main/)，sidecar [`coordinator-sidecar/`](../../../_tmp/a52b/coordinator-sidecar/)，A5.2a 回归 [`coordinator-a52a/`](../../../_tmp/a52b/coordinator-a52a/)。开发期采集 `delivery-main/` 等保留作对照：协调者用同一汇总器分别汇总两次采集，去掉 ID、时间戳、路径与哈希后**零差异**。合并后证据归档在主仓 `_tmp/archive/wt-a52b-tmp/a52b/`。

## 预注册、复现与隔离

沿用 exp014 的观察先行、三面独立、固定次序重复和失败对照方法。第一份 [预注册](../../../scripts/a5-19/preregistration-b.mjs) 在首次 B 测量前写好；30 个主例之后，为覆盖正确重绑后的 registry 分支和两个答复均在飞的分支，另在首次扩展测量前写好 [增补次序](../../../scripts/a5-19/preregistration-b-extra.mjs)。原预注册文件在采集后未修改；增补没有预填终态、改变 k 或放松规则。每次 launch/manifest 都保存规则哈希，实际 outcomes 只来自原始证据。

**原规则已经作为 PR #111 的第一个提交单独提交**（rebase 合并会改哈希，以提交内容为准），该提交只包含两个预注册声明和 `rules-b.mjs`；正式复采记录为 COMMITTED_BEFORE_COLLECTION，之后的提交才加入装置与报告。原文“先提交规则仍待协调者”已经过时。第 2 轮另在首次实测前写入独立 [生产时序预注册](../../../scripts/a5-19/preregistration-b-production.mjs)，原声明和检查器均未修改。协调者已按顺序执行：先单独提交这份增补声明，再用最终装置采集到 [`coordinator-production/`](../../../_tmp/a52b/coordinator-production/)（记录为 COMMITTED_BEFORE_COLLECTION），最后提交装置与报告；8 组仲裁投影（newRunHit / newRunAborted / newRunEntered / aborts）与 worker 的 `round2-production-final/` 逐项一致。具体哈希、顺序与验证见 [`_tmp/RESULT.md` 的第 2 轮](../../../_tmp/RESULT.md)。

```sh
# worker 可运行：不 listen、不重启 gateway；每次换不存在的 output
python3 scripts/a5-19/run-b.py --output _tmp/a52b/reproduction
python3 scripts/a5-19/run-b.py --sidecar --output _tmp/a52b/reproduction-sidecar
python3 scripts/a5-19/run.py --output _tmp/a52b/reproduction-a52a
python3 scripts/a5-19/run-production-b.py --output _tmp/a52b/reproduction-production-close

# 只交协调者在沙箱外运行，本轮没有执行
python3 scripts/a5-19/run-external-b.py --coordinator --suite cold \
  --output _tmp/a52b/coordinator-cold
python3 scripts/a5-19/run-external-b.py --coordinator --suite ws --port 48789 \
  --output _tmp/a52b/coordinator-ws
```

入口固定 Node 22、基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。Python 在 Node 启动前把 HOME、XDG_CONFIG_HOME、XDG_CACHE_HOME、XDG_DATA_HOME、TMPDIR、PILOT_HOME 指向 output 内独立目录，仅继承 PATH/语言设置，并设置两个 SKIP_BROWSER_OPEN。没有 npm install、下载、用户 home 写入、端口查杀或用户进程清理。核心装置禁止 listen；sidecar preload 同样禁止 listen，并逐个记录实际加载的文件哈希。WS 入口仅在协调者模式允许声明的 40000–59999 端口、只绑 127.0.0.1；端口被占即失败，不清理他人 owner。进程清理仅使用 launcher/stdio provider 自己 spawn 的对象与 PID。

新增文件都在 `scripts/a5-19/`：复用已有独立实验位置，避免触发 gateway-parity 已冻结装置白名单。没有改 specs、冻结脚本、白名单、基线源码或 state。实验仍不是冻结 E1 scorer，不能把本轮等同于 `session_domain.py --suite calibrate` 或完整 A5.2 完成。

## 装置与三个观察面

真实链为 `GatewayWsConnection → InProcessGateway → SessionRouter → AgentSession → TurnRunner`。两个文本 peer 各自使用真实 hello、真实生成的 connectionId/generation、同一生产 gateway 的请求分派器；没有模拟 session busy、binding 拒绝、权限 consume 或 registry 状态机。**原 close 夹具除了外部 loop 等待，还在人为加入的 gateway 方法 yield 点上等待**，原文“只有外部 loop/交互 producer 采用受控等待”不准确。

| 等待 / 次序点 | 性质与作用 | 适用证据 |
|---|---|---|
| `runtime-b.mts` 外部 loop 的 `hold` / `returned`；交互 producer 注册 pending | 替代外部 agent/model/tool producer 的受控等待；生产 AgentSession/TurnRunner 本来就异步消费 loop。两个 gate 用于保持执行/返回，非真实 provider 延迟测量 | 主夹具和第 2 轮均保留；loop 忽略 abort signal，不能据最终 completed 证明取消无效 |
| `fixtures-b.mts:76–79`，`yield* submit(input)` 后 `await streamReturn.promise` | **人为插入 yield 点**；生产 submit 已返回，装置额外持住 WS iterator，使 `inFlightSessions.delete` 延后 | 仅原 `close/no-pending/new-turn`；第 2 轮完全去除，直接使用生产 submitTurn 方法 |
| `fixtures-b.mts:85`，调用 disconnect 前 `await closeGate.promise` | **人为插入 yield 点**；生产 disconnect 是同步方法，不能在该方法执行前暂停并等多个 B RPC | 原 pending before-disconnect；第 2 轮改为 B 先 reconnect/resume，再 A close |
| `fixtures-b.mts:87`，真实 disconnect 后 `await closeGate.promise` | **人为插入 yield 点**；延长返回已捕获的 preserveTurn 值，原先的同步返回被变成 Promise | 原 pending after-disconnect 和 no-pending；第 2 轮保持同步返回，记录后直接 return |
| 生产 `GatewayWsConnection.handleClose:51–58` 的 `await disconnectInteraction`、顺序 `await abortTurn` | **生产中存在的 yield 点**；即使 disconnect 同步，调用方 await 仍有微任务切换；abort 等真实 router/fence 完成 | 第 2 轮原样保留；多会话在首个 abort drain 上等待，不改 abort 方法加 gate |
| 真实 registry.accept / append 和 session store 的文件操作 | **生产中存在的 I/O yield 点**；FileTap 调用真实文件函数，journal/故障注入在操作内部；成功写仍有真实 commit | 原存储故障与第 2 轮；第 2 轮无 I/O 延迟注入、无文件操作之外的 hold |
| 第 2 轮 terminal rename commit / router.endTurn 观察回调 | **同步投递探针，无新增 yield**；立即从外部 driver 发 B submit，单会话于 endTurn 发 close；不等待回调返回条件 | 用于尽早投递竞争请求，非真实网络事件自然到达的证明；endTurn/WS finally 不被持住 |
| driver 的 journal/RPC 等待、sidecar Module callback/外部 iterator 暂停、cold 的父进程 READY | 外部 producer/consumer 或驱动器 barrier；Module RPC 是真实已有异步等待，暂停消费者是明确的外部消费次序 | 不属于 close 清理中的新增 yield；sidecar timeout callback 被显式触发，非自然 deadline 分布 |

表中的 `barrier` 日志本身是同步记录；是否改变生产时序取决于相邻方法有没有额外 `await`，不能只凭日志名认定是生产等待点。20 秒 watchdog 只查挂起，不决定交错。

- **帧面**：每连接独立保留 sendText 的原文与解码值、request id、seq、final、notification、response。FRAME_RAW 对原文逐项核验；FRAME_SEQ 对每请求核验 seq 连续及包装 final。WS final 重复承载业务终态只算包装，不当作第二个 durable 终态。此处是传输中立文本通道，close 后仍能受控投递迟到旧 binding 调用并记录 sendText；**不证明关闭的真实 TCP socket 能收到这些帧**。真实 WS cross-check 已由协调者采集，双录面的跨遍汇总待 #112；不覆盖本轮 close 仲裁或真 Web bridge。
- **session 存储面**：单个真实 JsonlTranscriptWriter / JsonlSessionEventStore / JsonlSessionPersistence；在 `node:fs/promises` 的真实 appendFile/readFile 边界切 EIO。成功写仍调用原方法；部分写先真的写半段字节再抛错。独立读文件原文、解析坏行、调用真实 store.read，分别核验原文、完整 entries、成功写 journal、FIFO 与 reader diagnostics。尝试、成功调用和可被 reader 解析的行分别记录。
- **registry 面**：显式注入生产默认类 FileRunRegistry，真实 mutationChain、文件锁、refresh、snapshot writeFile 与 rename 均执行。tap 仅作用于夹具自有路径。read/write EIO、半个临时快照、rename 前失败、真实 rename 完成后返回 EIO 分开测。故障时保留目标文件和 `.tmp` 的真实字节；再用 fresh FileRunRegistry get/events 与直接读盘核验 records/events/seq/state/revision/lastSeq/result。rename 后报错的已落盘内容不会被误删成“失败未写”。

barrier 和受控 Promise 决定次序；20 秒 watchdog 只检测装置挂起。每个 accepted registry 操作分配独立 journal identity，重复 accept 的无 persist 分支明确记录 skipped，避免把两个同名调用误当同一次操作。多会话只核同连接全序，跨连接的全局 I/O 到达先后不作为语义差异。

k=5 比较保留完整帧 payload、session entries/diagnostics、registry records/events/result 和答复/abort 归属。仅归一化本次目录、registry 墙钟/PID、诊断 errorId 与连接随机身份；connectionId 按真实 peer 一一映射，generation 保留相对差值，不直接删除。sidecar 的 instance/generation/stream 随机值也按出现次序一一映射，比较完整三面及 close 后丢弃的 enqueue。原值始终留存。**一致只表示同一受控路径可重复，不表示自然调度稳定性或生产发生率。** 本次无 unstable 组，无空组或隐式 skip。

## 双连接与多会话结果

`C=turn_completed(completed)`，`U=error(result_unknown)`；表中省略正常 submit 前缀，原文保留。未标注状态写的普通轮次 transcript 为一组 turn_started / accepted_input / turn_result。

| 子例 | 实时 / 查询观察 | durable session | registry 观察 | 重复 |
|---|---|---|---|---|
| permission / owner-first、other-first | 当前 owner A delivered=true，B=false；B 即使 false 也收到自己 RPC 引出的 permission_changed(resolved) | 一组普通轮次，无答复被额外当作 status 写入 | request event 后最终 C；completed，revision=6、lastSeq=5 | 每例 5/5 |
| question / owner-first、other-first | A=true，B=false；两侧无 permission notification；B snapshot 在答复后仍包含原 elicitation_request 历史事件 | 同上 | 同上 | 每例 5/5 |
| permission / question handoff-double | B 带 A 的精确 previousBinding 重绑；A 旧答复=false，B 新答复=true，B 重复答复=false；真实 resolve 各一次 | 同上 | 同上 | 每种 5/5 |
| permission / question raced-double | A、B 两个答复发出后才等待两边响应；A=true、B=false，resolve 一次。JS admission 固定为 A 先、B 后 | 同上 | 同上 | 每种 5/5 |
| SDK control | 独立 SDK 控制连接 B 不带 binding 的 permission 答复=true，A 随后双答=false | 同上 | 同上 | 5/5 |
| distinct-run / same-run，未重绑 | B submit 均先收到 interaction_reconnect_required；没有进入 registry 重复接纳或 session busy 分支 | 只有 A 的一轮 | 只有 run；completed，revision=5、lastSeq=4 | 各 5/5 |
| 正确重绑 / same-run | B 回放当时的三帧前缀，WS 包装 final=U；A 后来收到 C，B 原 iterator 没有再被推送 C，后续 run_events 可读到 C | 一轮 input / result，loop 只执行一次 | 同一 run 最终 completed，revision=5、lastSeq=4 | 5/5 |
| 正确重绑 / distinct-run | B 收到 agent_status(session_busy) → error(session_busy) 及相同包装 final；A 完成 C | 原 session 写入 session_busy status，A 普通轮次完成 | run completed；other-run failed / result=error(session_busy) | 5/5 |
| 两 session / A-first、B-first | 两个 loop 同时在飞，按 barrier 指定完成先后；各连接帧、run id 不串；跨 session 查询可见真实目标，错误 permission 答复=false | session-A 只有 fixture-A status，session-B 只有 fixture-B，各有自己的轮次 | 两 record，各自 seq 1–4、completed、revision=5、lastSeq=4 | 各 5/5 |

permission 成功后 B 的 permission_list 从一个 pending 变成空，证明另一连接查询可见 consume 后状态；notification 仅由各自 RPC 分派引出，**resolved notification 不能证明 delivered=true，也没有测出跨连接广播**。question 的历史 snapshot/registry event 不会因答复变成“已答”标记；B 可通过后续终态查询看到完成，不能仅凭历史 request event 判断仍 pending。

协议没有单独的“订阅 registry”方法：本轮测两边 submit 尝试、run_events / run_reattach / active_turn_snapshot 读面。正确重绑后同 run 的第二个 submit 是有界快照回放，实测不会成为跟随 A 的实时订阅。不能把第二连接只读到前缀写成“两个实时订阅都完成”。

## 旧 close / 新 reconnect 交错

原受控夹具结果仍留存，不能把装置路径改写成生产发生事实。第 2 轮原样回归这五个 close 子例，均仍 5/5 复现；主装置 34 组/170 次、10 个对照全部通过。

| 原 barrier 次序 | 人为 hold 的影响 | binding / 旧 close 动作 | 第 2 轮核对 |
|---|---|---|---|
| pending permission / question；close 已进入、停在 disconnect **前**，B reconnect + resume + 答复 | 同步方法前新增 yield；让完整 B RPC 链可插入，原叙述的入场次序依赖 hold | A=false，B=true；迟到 disconnect=false、preserveTurn=false，abort 旧 run | 无 hold 的 **B reconnect/resume → A close → 答复**，两种各 5/5 得到相同 binding/abort 结果；支持结果机制，不支持原“close 已进入再暂停”次序 |
| pending permission / question；真实 disconnect 已执行，返回值被持住，B 完成多个 RPC | 同步返回变成 Promise，扩大捕获 preserveTurn 后的窗口 | A=false，B=true；先捕获 preserveTurn=true，最终无 abort | 无 hold 的 **A close/sync disconnect → B reconnect/resume → 答复**，两种各 5/5 得到相同结果；无需延后 disconnect 返回 |
| 无 pending；gateway 完成、**WS return 被持住**；B 新 run 在飞再放旧 close | 两处 hold 共同保留 A 的 stale inFlight 条目，并延后 close 的 abort | 无 runId 的 abort 实际 activeRun=new-run，signal 到新 loop；受控 loop 仍 C | 原例仍 5/5，但去除 hold 后单会话两种次序均 0/5 命中新 run；降级为机制证据 |

### 第 2 轮生产清理时序对照

入口为 [production-close-b.mts](../../../scripts/a5-19/production-close-b.mts)，独立预注册 8 组。submitTurn 使用生产原方法；disconnect 的同步日志包装调用原方法并直接返回同一值，断言返回值不是 Promise。router.begin/end、WS Set.delete 和真实 router.abort 仅同步记录，不增加等待。B 使用真实 A binding 重绑并 resume；accept 内的真实 FIFO、文件锁、写入和 rename 不绕过。无需注入 I/O 延迟即可完成所有指定次序。

| 生产清理对照次序 | abortTurn 实际命中 runId | WS / registry 关键观察 | k=5 |
|---|---|---|---|
| 单会话：old endTurn 后同栈立即发 B 新 submit 和 A close | 每次调用时 activeRun **为空**；没有 new-run 命中 | A finally 清理先于 B beginTurn；B 实际接纳并进入新 loop，未收到 abort signal | 0/5 命中新 run |
| 单会话：在旧终态真实 registry rename commit 就提前发 B submit，old endTurn 时 close | 每次 activeRun **为空**；没有 new-run 命中 | B accept 已排队，但真实 mutationChain / I/O 后续晚于 A finally 清理；新 loop 确实执行 | 0/5 |
| 多会话：A first-session 排在目标前；close 等 first-run 的真实 abort drain；目标在 endTurn 发 B submit | 仅命中 **first-run** | B new-run 在首个 abort 仍等待时确实进入；目标 A inFlight 已被 WS finally 删除；close 到目标时虽 activeRun=new-run，has(session)=false，跳过 abort | 0/5 |
| 多会话：同上，改在目标旧 terminal rename commit 提前发 B submit | 仅命中 **first-run** | 同样真实启动新 run 后才释放 first-run；close 到目标时无 stale inFlight 条目 | 0/5 |
| permission / question：reconnect-first | 命中 **旧 run**（即 B 已接管的同一 run；**基线缺陷候选**，见表下） | disconnect=false、preserveTurn=false；旧 A 答复 false、新 B true；resolve 各一次 | 每种 5/5 |
| permission / question：close-first | **未调用 abort** | 同步 disconnect=true、preserveTurn=true；旧 A false、新 B true；resolve 各一次 | 每种 5/5 |

可逐项核对的第 1 次 `terminal-commit` 证据：

- [单会话 journal](../../../_tmp/a52b/round2-production-final/raw/measurements/production-close/single/terminal-commit/1/timeline.ndjson)：order 53 旧终态 rename commit barrier → 54 发起 B → 59 old endTurn → 63 abort.input（activeRun 字段缺省，即无活动 run）→ 65 A WS finally delete → 73 B beginTurn(new-run)。这是新接纳未赶进清理窗口的证据。
- [多会话 journal](../../../_tmp/a52b/round2-production-final/raw/measurements/production-close/multi/terminal-commit/1/timeline.ndjson)：81 abort first-run → 101 目标 old endTurn → 103 A WS finally delete 目标 → 110 B beginTurn(new-run) → 138 新 loop 在飞 → 153 才 disconnect 目标（activeRun=new-run，但 A inFlight 为空）→ 154 close 完成。没有针对目标的 abort.input。

**基线缺陷候选（生产次序下可复现）**：reconnect-first 中，B 已经 reconnect 接管 A 的交互，A 的 submit 流仍在飞，A close 时 `disconnectInteraction` 返回 preserveTurn=false，`GatewayWsConnection.ts:57-58` 随即按 sessionKey 调 abortTurn——中止的正是 B 刚接管、用户正在看的那个 run。这不是夹具人为次序（本轮生产时序装置 5/5 复现）。是否属 bug（意向书 bug 定义）与 Rust 宿主如何处理，留 A7 判定并按 D4 同类流程交人，本实验不替人定。

因此，原装置证明按 session、无 runId 的 abort 在保留 stale 条目时能指向新 run；**成功清理 / FileRunRegistry 的单会话生产次序未达到该机制，结论为「机制存在、指定生产次序未达到；源码显示成功路径窗口只有微任务（单会话）」**。多会话实测也未命中：等待另一个会话并不足以保留目标的旧 inFlight 条目。上述是两个指定次序各 k=5 的仲裁结果，不能外推为所有 Gateway 配置和异常路径永不可达；本轮未覆盖失败/timeout 的 post-agent snapshot I/O、无 registry 配置或真实网络自然调度。同步观察回调立即投递是最早投递探针，未暂停原清理栈，也不代表真实 socket 的到达方式。

受控外部 loop 忽略 signal，最终 completed 是装置行为，不能据此证明真实模型/工具取消效果。真实 WS 关闭与 Web bridge 保存/重放 binding 仍需对应证据；本轮未 listen。40 次原始三面观测与 SHA-256 在 [报告](production-close-results.json)，源文件加载前后哈希均一致。第 2 轮正式工作树采集为 [`round2-production-final/`](../../../_tmp/a52b/round2-production-final/)，原夹具回归为 [`round2-original-regression/`](../../../_tmp/a52b/round2-original-regression/)；Git 预提交后的正式采集为 [`coordinator-production/`](../../../_tmp/a52b/coordinator-production/)，结果与之逐项一致。

## 存储故障结果

全部子例各 5/5；session 故障使用 record_agent_status_message RPC，registry 接纳/终态故障经过真实 submit_turn，读故障经过 run_get。目标字节、临时文件和解除故障后的查询见逐例 raw 的 recovery 字段。

| 故障点 | 实时错误面 | durable / registry 故障后读面与恢复 |
|---|---|---|
| session appendFile 前 EIO | fault-status RPC：ok=false、code=EIO；recovery RPC：recorded=true | seed → recovery 两条完整 status；失败 status 不在读盘 entries 中 |
| session 半行 append 后 EIO | 同上；后续 recovery RPC 仍 recorded=true | 同一个已准备 tail 的 provider 把 recovery 接到半行后；fresh reader 只有 seed，line 2 为 transcript_line_invalid；成功 append 调用不等于完整可恢复行，坏字节完整保留 |
| session readFile EIO | reader 抛 EIO，独立 failure journal 留存 | 去掉一次性故障后读回 seed → recovery，无坏行 |
| registry accept read/write EIO | submit：U（业务 error + WS final 包装） | 原接纳未确认；解除故障后同 run 重新接纳并完成，最终 completed/C、revision=5、lastSeq=4 |
| registry accept 临时快照半写 / rename 前 EIO | 同上 | 临时残片/完整 `.tmp` 保留；目标未提交；恢复 submit 实际执行一轮并完成 |
| registry accept **rename 完成后** EIO | 原 submit：U；再次 submit 无业务帧，只产生 WS final U | 磁盘已有 accepted、revision=1、lastSeq=0，无 result；同 run 被当 duplicate，不重执行；durable session 明确为空，未创建 AgentSession |
| registry terminal append write EIO / 临时半写 / rename 前 EIO | 前缀后三种都只收到 U 及包装 final | session 已有 turn_result；registry 仍 running、revision=4、lastSeq=3、result 缺省；读 API 如实呈现分歧，不把 session 结果当 registry 成功 |
| registry terminal append **rename 后** EIO | 仍收到 U 及包装 final | session 有 turn_result；registry 实际 completed/C、revision=5、lastSeq=4；失败返回不能抹掉实际 rename commit |
| registry readFile EIO | run_get：ok=false、code=EIO | 一次性故障解除后 fresh get/events 和 RPC 可读 running 前缀；释放 loop 后正常 C |
| registry 损坏 JSON 快照 | run_get：ok=false、code=gateway_request_failed，Unexpected end of JSON input | 原坏字节保留；仅恢复本夹具故意破坏前的快照以测读恢复，随后查询/轮次可完成；**不冒充基线能自行修复坏快照** |

rename 测的是操作前与真实操作完成后两个边界：没有模拟 OS 原子 rename 的“半个命名空间状态”，没有电源断电/fsync/crash-consistency 证明。session 部分写针对同一 provider 的继续追加；未把重建对象的读面当真实进程重启。

## 真实 sidecar G 路径

5/5 使用基线已有 `dist/src/cli/pilotdeck-agent-loop-sidecar.js` 和生产 stdio connection/runtime factory。宿主通过 createAgentSession 的正式 agentLoopFactory 入口组成；真实 model Module port 被 barrier 持住，gateway timeout callback 显式触发，真实 cancel 请求到达 sidecar。独立 stdout tap 观测 cancel ACK 后才释放 model；生产 client 此时正等待该 model callback，不能等待同一个 client 的 receive 消费 ACK，否则会制造装置死锁。没有手工抛 AgentLoopResultUnknownError，没有重启/杀死 sidecar 来制造 G。

| 观察面 | 5 次实测 |
|---|---|
| 实时 submitTurn | 正常前缀 → agent_status(turn_timeout) → error(turn_timeout)；G 的 status/error 只在 closed queue 上尝试入队，客户端没有收到 |
| durable | status 顺序 turn_timeout → gateway_submit_failed；生产 operation ledger 同时有 started、accepted、terminal(result_unknown)，code=DEADLINE_EXCEEDED、lastAppliedSequence=2，未知事实未改成 success |
| registry | 特意选 exp014 的 no-registry / fast / model 格；未配置、不适用，没有借此代替默认 registry 格 |
| 身份/退出 | 五个实际 child PID、hello/capabilities、model Module call、cancel ACK、真实 unknown final、所有已加载 child 文件哈希、code=0 的退出均保留 |

这确认了 exp014 至少一个 G 格的真实 sidecar 可达性，处理 #105 第 5 条。模型是宿主受控 provider，没有调用外部模型服务或真实工具；未扩成“真实 registry 六格、工具等待、自然 deadline 调度、历史 #19 全部归因已经完成”。

## 失败对照层级与待采集部分

| 本轮对照 | 层级 / 指定命中 | 实际结果 |
|---|---|---|
| apparatus-bypass-status-fifo | 装置真绕过 writer FIFO，把接纳 one/two 反序发到真实 store；STATUS_FIFO | 检出，坏顺序与真实文件留存 |
| apparatus-bypass-registry-fifo | 装置真把接纳 one/two 反序交给真实 FileRunRegistry；REGISTRY_FIFO | 检出，真实快照与 journal 留存 |
| apparatus-omit-B-connection | 不创建 B，实际只驱动 A；CONNECTIONS | 检出，不能单连接冒充双连接 |
| apparatus-old-binding-as-new | 消费路径故意把 A 的旧 binding 替成 current B；BINDING_IDENTITY | 检出，即使底层因此实际 consume 成功也不能假通过 |
| drop-frame / both-sides-bad | 独立观察副本 / 两份相同坏副本；FRAME_RAW | 均检出 |
| omit-registry-events / ignore-result-overwrite | 副本漏 event / 错 result 投影；REGISTRY_RAW | 均检出；真实 T→G result 覆盖的旧对照另按原 A5.2a 原样回归 |
| attempt-as-success | 同时伪造 decoded、rawText、声称的 fresh reader；STATUS_COMMITS | 检出，独立成功写 journal 未被伪造 |
| double-answer-success | 副本把真实 consume=false 改为 delivered=true；ANSWER_JOURNAL | 检出 |

原 A5.2a 的 9 个对照原样回归，包含丢 timeout、漏 registry events、忽略真实 result 覆盖、status/registry 尝试当成功、两侧同坏、删 durable status、反序 status/registry journal；各自仍命中原规则 ID。新装置级对照会真的运行坏装置，不只修改 timeline 副本；检测使用同一三面规则检查器。普通异常、watchdog、非零退出码都不能算指定规则命中。

| 交协调者的部分 | 当前证据 / 交付边界 |
|---|---|
| cold/running、cold/unknown，各 k=5 | **协调者已实跑**（`_tmp/a52b/coordinator-cold/`，沙箱外，同一 PILOT_HOME、不同 PID 的 B）。**cold/running**：A 被杀前 registry 为 running；B 启动后 run_get / run_reattach 均为 `state=interrupted`（revision 5、lastSeq 3），run_events / resume / history / snapshot 均 ok。**cold/unknown**：重启后仍为 interrupted，`result` 保留 `result_unknown`（lastSeq 4）。两组各 5/5 一致（去掉 PID 与时间戳后）。unknown 初始事实由真实 registry.append 构造，不冒充 sidecar 执行事实；resume 观察到的是 no-op RPC，不宣称已 hydrate 新 AgentSession |
| 真实 WS 双连接 / 多会话，15 子例 × k=5 | **协调者已实跑**（`_tmp/a52b/coordinator-ws/`，127.0.0.1:48789，launcher 只回收自有子进程，exit 0）。15/15 子例、75 次全部执行，预注册哈希与进程内一致。**跨遍一致性与进程内结果的逐项对照尚未汇总**（WS 汇总器未实现，已开跟进，前置于 A5.4），本表不宣称其稳定或与进程内一致 |
| 真 Web bridge 重连 | **NOT_RUN**。上述协议 driver 不使用 UI bridge，不能替代 exp003 的 D5 自动恢复门；需要原 bridge 录帧与 reconnect→有效 answer→终态闭环 |
| 完整 E1 校准 / D4 评分 | 未接入冻结 scorer，未登记 accepted diff，未关闭 #19/#67 或替人定评分口径 |

全 workspace fmt/clippy/test、两套 check_frozen、ledger（105 个自检）、schema 与 diff 检查通过；test 为 820 passed / 0 failed / 2 个源码既定 ignored。完整命令、隔离与失败尝试、提交顺序和外部剩余项见 RESULT。协调者移除 worktree 前应归档证据；Cargo cache/target 是可重建验证缓存，不需混入原始证据档案。

## 2026-10-16 · #112 真实四格与历史 WS 汇总

四格补证入口仍为 `run-b.py --sidecar`，明确枚举 registry有/无 × model/tool，各k=5。每次实际到达目标 Module call/宿主barrier、消费者停在前缀后才人工触发宿主 timeout；真实cancel到达子进程并被ACK，随后释放宿主port，观察真实operation terminal，不预设unknown断言。tool由模型返回结构化call进入真实capability `execute_batch`（生产dispatcher同时支持execute）；宿主executeAll只持住barrier，不执行真实工具。20次均实际得到result_unknown/DEADLINE_EXCEEDED，model的lastAppliedSequence=2、tool=8。原始terminal与完整三面见 [sidecar-four-results.json](sidecar-four-results.json)。

无registry实时为前缀→T status/error；有registry实时只有前缀，T/G的enqueue均在closed queue后。四格durable status均T→G；有registry最终failed/G，model revision=8/lastSeq=7、tool=9/8，与exp014快格对应语义一致。真实loop额外operation ledger/模块事件均保留，**不把真实ledger与受控loop全文冒认相同**。四组各5/5完整投影稳定，registry投影只排除record的acceptedAt/updatedAt/ownerPid；随机sidecar instance/generation/stream仍按原有一一映射。20个自有child均code=0退出；不listen，未调用外部模型。

旧TIME=2026-10-15使今天的绝对operationDeadline在execute admission前已过期，首两次失败原件保留。修正只在本sidecar装置将生产gateway的`now`设为本次采集时间；该生产时钟同时决定operationDeadline和gateway status.createdAt，两类值保留原文并在manifest记录collectionTime，durable事件外壳/宿主依赖仍为原TIME，不归一全部时间。第三次在registry格暴露读面过早：session.submit退出先于gateway catch的registry append结算。现在等真实第二次queue.close后取三面，不增加生产路径等待。失败原件与最终采集均在 `_tmp/20261016-a5-112/`，未用admission失败替代目标阶段abort证据。

历史WS原件按精确15case×5汇总，75条各自与coordinator-main同case/同遍的完整语义投影一致，15组各5/5稳定。汇总器 [summarize-ws-b.py](../../../scripts/a5-19/summarize-ws-b.py) 只排除registry record及其RPC读面acceptedAt/updatedAt/ownerPid，并按已知peer的实际binding映射connectionId及generation相对差；保留其余ID、时间、完整帧payload、durable、registry events/result、answer与abort。5项负控证明错误runId、非registry时间和终态不能被归一吞掉。结果含每份observation/journal SHA，见 [ws-results.json](ws-results.json)。

WS是**同一launcher自有Node进程内75次观测**，不是75个独立进程；历史launch记exit0/reaped=true。历史WS manifest缺fixtureFiles，仅能绑定原始输入、共同源码哈希和预注册哈希，不能宣称为当前HEAD重新执行。cold/sidecar/main/旧a52原件均在`_tmp/archive/wt-a52b-tmp/a52b/`及`_tmp/archive/wt-a52-tmp/a52/final-run/`，本次不伪造新cold或WS运行数。

以上仅准备A5.4登记证据；未登记/冻结A19-HOST，未关闭#112或升级阶段/真人验收。
