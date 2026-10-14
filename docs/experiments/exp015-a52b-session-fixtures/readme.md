---
type: experiment
number: "015"
date: 2026-10-15
title: A5.2b · 双连接、交错 close、存储故障与重启夹具
status: 进程内实测完成；真实 sidecar G 交叉确认完成（一格）；cold-restart 与真实 WS 已由协调者实跑（WS 跨遍汇总待补，#112）
related: [plan/005_phase-a-session-domain, experiments/exp014-a52-session-fixtures, research/008_phase-a-session-domain/gap]
---

# A5.2b 基线受控实测

**34 个子例 × k=5 = 170 次进程内测量，全部一致；4 个装置级、6 个观察副本对照均命中指定规则。真实 stdio sidecar 另做 5 次 G 路径交叉确认；原 A5.2a 装置原样回归 360 次及 9 个原有对照。** 本轮共 535 次测量、19 个对照，不含任何 Rust 对拍分数。cold-restart（2 组 × 5，结果一致）与真实 WS（15 子例 × 5 = 75 次执行完，跨遍汇总待补）已由协调者在沙箱外实跑，结果见下表；真实 Web bridge 重连仍单列为未验，不能据此关闭 D5。

逐例摘要、完整集合、装置与证据哈希见 [results.json](results.json)。**正式采集**（预注册规则先单独提交（本 PR 第一个提交；rebase 合并会改哈希，以提交内容为准），记录为 COMMITTED_BEFORE_COLLECTION）：进程内 [`_tmp/a52b/coordinator-main/`](../../../_tmp/a52b/coordinator-main/)，sidecar [`coordinator-sidecar/`](../../../_tmp/a52b/coordinator-sidecar/)，A5.2a 回归 [`coordinator-a52a/`](../../../_tmp/a52b/coordinator-a52a/)。开发期采集 `delivery-main/` 等保留作对照：协调者用同一汇总器分别汇总两次采集，去掉 ID、时间戳、路径与哈希后**零差异**。合并后证据归档在主仓 `_tmp/archive/wt-a52b-tmp/a52b/`。

## 预注册、复现与隔离

沿用 exp014 的观察先行、三面独立、固定次序重复和失败对照方法。第一份 [预注册](../../../scripts/a5-19/preregistration-b.mjs) 在首次 B 测量前写好；30 个主例之后，为覆盖正确重绑后的 registry 分支和两个答复均在飞的分支，另在首次扩展测量前写好 [增补次序](../../../scripts/a5-19/preregistration-b-extra.mjs)。原预注册文件在采集后未修改；增补没有预填终态、改变 k 或放松规则。每次 launch/manifest 都保存规则哈希，实际 outcomes 只来自原始证据。

**本 worker 按任务禁止提交。严格“先提交规则、后采集”的 Git 时序仍待协调者完成：第一提交只含两个预注册声明及 rules-b.mjs 机器检查器，随后按最终装置复采，再把装置、文档和复采结果放第二提交。** 把机器断言也纳入首提交，防止只封存说明、检查器却在采集后改变。本文当前证据是工作树先登记后采集，不冒充已完成的 Git 预提交。具体顺序、哈希和验证输出见 [`_tmp/RESULT.md`](../../../_tmp/RESULT.md)。这保留了 #105 第 3 条的边界。

```sh
# worker 可运行：不 listen、不重启 gateway；每次换不存在的 output
python3 scripts/a5-19/run-b.py --output _tmp/a52b/reproduction
python3 scripts/a5-19/run-b.py --sidecar --output _tmp/a52b/reproduction-sidecar
python3 scripts/a5-19/run.py --output _tmp/a52b/reproduction-a52a

# 只交协调者在沙箱外运行，本轮没有执行
python3 scripts/a5-19/run-external-b.py --coordinator --suite cold \
  --output _tmp/a52b/coordinator-cold
python3 scripts/a5-19/run-external-b.py --coordinator --suite ws --port 48789 \
  --output _tmp/a52b/coordinator-ws
```

入口固定 Node 22、基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。Python 在 Node 启动前把 HOME、XDG_CONFIG_HOME、XDG_CACHE_HOME、XDG_DATA_HOME、TMPDIR、PILOT_HOME 指向 output 内独立目录，仅继承 PATH/语言设置，并设置两个 SKIP_BROWSER_OPEN。没有 npm install、下载、用户 home 写入、端口查杀或用户进程清理。核心装置禁止 listen；sidecar preload 同样禁止 listen，并逐个记录实际加载的文件哈希。WS 入口仅在协调者模式允许声明的 40000–59999 端口、只绑 127.0.0.1；端口被占即失败，不清理他人 owner。进程清理仅使用 launcher/stdio provider 自己 spawn 的对象与 PID。

新增文件都在 `scripts/a5-19/`：复用已有独立实验位置，避免触发 gateway-parity 已冻结装置白名单。没有改 specs、冻结脚本、白名单、基线源码或 state。实验仍不是冻结 E1 scorer，不能把本轮等同于 `session_domain.py --suite calibrate` 或完整 A5.2 完成。

## 装置与三个观察面

真实链为 `GatewayWsConnection → InProcessGateway → SessionRouter → AgentSession → TurnRunner`，只有外部 loop/交互 producer 采用受控等待。两个文本 peer 各自使用真实 hello、真实生成的 connectionId/generation、同一生产 gateway 的请求分派器；没有模拟 session busy、binding 拒绝、权限 consume 或 registry 状态机。

- **帧面**：每连接独立保留 sendText 的原文与解码值、request id、seq、final、notification、response。FRAME_RAW 对原文逐项核验；FRAME_SEQ 对每请求核验 seq 连续及包装 final。WS final 重复承载业务终态只算包装，不当作第二个 durable 终态。此处是传输中立文本通道，close 后仍能受控投递迟到旧 binding 调用并记录 sendText；**不证明关闭的真实 TCP socket 能收到这些帧**。真实 WS cross-check 另有 GatewayServer + 网络客户端的双录面，待协调者采集。
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

| barrier 次序 | binding / 答复 | 旧 close 实际动作 | 存储 / registry | 重复 |
|---|---|---|---|---|
| pending permission / question；旧 close 阻塞在 disconnect **之前**，B 先 reconnect + resume + 答复 | 旧 A=false，新 B=true | 迟到 disconnect 返回 disconnected=false、preserveTurn=false；旧 close 调用无 runId 的 abort，activeRun=run | 受控 loop 忽略 abort 并由 barrier 完成，普通 transcript，registry completed/C | 每种 5/5 |
| pending permission / question；真实 disconnect 已执行，返回值阻塞，B reconnect + resume + 答复 | 旧 A=false，新 B=true | disconnect 先捕获 preserveTurn=true，旧 close 释放后未 abort；B binding 保留 | 同上 | 每种 5/5 |
| 无 pending；A gateway 已完成，**A WS submit return 尚被持住**；B resume + 新 submit 在飞，再释放旧 close | 新 run 接纳，旧 binding 不参与新 run 接纳 | 旧 close 的 abort.input 只有 sessionKey，实际 activeRun=new-run；新 loop 收到 abort signal | 两轮 transcript；受控 loop 仍 C，两个 registry record 均 completed/C | 5/5 |

这证明特定次序下旧 close 会按 session 指向新 run，**没有证明真实模型/工具被成功取消，也没有替产品定修法**：受控 loop 特意保留 return barrier、忽略 signal，最终 completed 是该装置的观察值。旧 handle fence、interaction generation 和 gateway run 身份分别留证，未混成同一种 generation。真实 WS 关闭会丢弃对旧 socket 的发送，真实 bridge 保存/重放 binding 仍需单独测。

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
