---
type: research
number: "008"
date: 2026-10-14
title: 阶段 A 深水区：状态、91 个 method 与宿主对账缺口
tags: [phase-a, inventory, state-authority, deadline]
status: 静态定位；未运行基线；未解决项逐项保留
related: [research/008_phase-a-session-domain/must-know, research/008_phase-a-session-domain/decision, research/005_phase-a-gateway, plan/005_phase-a-session-domain]
---

# 008 · 差距与证据

**最大缺口在 Rust 宿主能力与统一状态权威，而非再补 91 个 dispatch 分支。** 当前 gateway 本地白名单为空；现有 sidecar 仍发 `module_call` 给宿主。证据：`src/pilotdeck-gateway/src/dispatch.rs:1-21`、`src/pilotdeck-transport/src/stdio.rs:469-485`。本目录 `B/` 根和事实/建议区分见 [must-know.md](must-know.md)；本仓快照为 `07364f8`，下列“已有”只指该快照可读源码或已入仓报告，不指本轮实测。

## 一、起点不能沿用旧计划的完成时态

| 项 | 核对结果与差距 | 证据 | 下一步 |
|---|---|---|---|
| A2/A3 | A2 已有冻结登记，A3 schema 已有独立冻结；exp006 的早期 DRAFT 不能覆盖其 r3 结果。新会话场景仍不在八组证明范围。 | `specs/gateway-parity/FROZEN.md:3-6`、`specs/gateway-parity/schema-FROZEN.md:1`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:74-78,97-98` | A5 在批准后扩装置；保留原组回归，不能把它当会话全量证据。 |
| A4 | 本地 method 白名单仍空，源码还断言只读方法全透传；不能据“有 dispatch”写成 10/10 已迁。 | `src/pilotdeck-gateway/src/dispatch.rs:6,43-59` | 实施时复核 A4 成果；A5 调研/装置可先做，原生默认切换不得绕过 A4。 |
| method 分类 | 本轮使用 10/46/4/41；005 的部分摘要仍写 15/36。 | `docs/research/005_phase-a-gateway/method-inventory.md:13`、`docs/research/005_phase-a-gateway/README.md:16` | 以下表是迁移工作包映射，不改冻结类别、分母或原文。 |
| list_sessions | 冻结分类保守归 (b)，但实际 createLocalGateway 注入 registry.listSessions → sessionCatalogConsumer；并非所有部署都直接读 Router Map。 | `B/src/gateway/SessionRouter.ts:368-374`、`B/src/cli/createLocalGateway.ts:827`、`B/src/cli/ProjectRuntimeRegistry.ts:1703-1705` | 移植真实注入路径，保留默认 Router 分支测试；不借此单独重分类。 |
| 真实 UI 重连 | exp003 记录 bridge 丢 binding，没发恢复请求；exp006 保存 binding 的协议驱动只有回放，答复仍 false。 | `docs/experiments/exp003-transparent-proxy/readme.md:55,65-70`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:78` | D5 等人定 UI 闸门的范围；Rust 协议层正常并不能修客户端未发请求。 |

## 二、(b)46 与 (c)4：完整覆盖，按状态所有者建、整域启用

下表每个 method 恰好出现一次，共 50 个；“A7”是同一会话域 issue 内的工作包，不是允许逐 method 切到生产。实现映射依据为原清单 `docs/research/005_phase-a-gateway/method-inventory.md:23-123`；所有者的直接证据列在末列。

| 包 | method（数量） | Rust 工作与必须一起验证的状态 | 直接证据 |
|---|---|---|---|
| C · 连接/交互（4） | `submit_turn`、`reconnect_interaction`、`elicitation_respond`、`permission_decide` | A7：hello/auth/binding 与下列会话域共享 authority；SDK 控制连接单独测 | `B/src/gateway/server/GatewayWsConnection.ts:123-169,450-505` |
| S1 · 会话目录/生命周期（9） | `list_sessions`、`resume_session`、`new_session`、`close_session`、`delete_session`、`rename_session`、`tag_session`、`close_project_sessions`、`record_agent_status_message` | A7：session catalog 与 live Router 分支；close/delete 排空写者，status 不绕过 live owner | `B/src/gateway/client/InProcessGateway.ts:2022-2078,2211-2239`、`B/src/gateway/SessionRouter.ts:203-211,327-337` |
| S2 · 在飞轮次（4） | `steer_turn`、`cancel_steer`、`abort_turn`、`active_turn_snapshot` | A7：runId 绑定、mailbox、abort drain、replay 截断/终态保留 | `B/src/gateway/client/InProcessGateway.ts:1859-1893,2000-2020,2479-2481`、`B/src/gateway/client/GatewayTurnReplayStore.ts:8-10,60-88` |
| S3 · 历史事务（6） | `export_session_transcript`、`restore_session_transcript`、`read_session_messages`、`fork_session`、`replace_last_turn`、`finalize_last_turn_replacement` | A7：读 epoch、busy、写 reservation、claim/commit/rollback、恢复重试；读与写共用 A6 存储 | `B/src/gateway/client/InProcessGateway.ts:2241-2257,2841-2889`、`B/src/gateway/client/GatewayTurnReplacementCoordinator.ts:57-75,131-177,179-207`、`B/src/cli/createLocalGateway.ts:1162-1167,1315-1318` |
| S4 · 权限与 dialog（6） | `permission_list`、`grant_session_permission`、`user_dialog_list`、`user_dialog_claim`、`user_dialog_release`、`user_dialog_respond` | A7：权限规则、一次性消费、dialog 校验/lease、hosted/recovered 优先级、跨连接广播 | `B/src/gateway/client/InProcessGateway.ts:2638-2792,2837-2839`、`B/src/gateway/client/GatewayInteractionCoordinator.ts:108-136` |
| S5 · 会话配置（7） | `session_model_set`、`session_model_clear`、`set_permission_mode`、`set_session_thinking`、`output_styles_list`、`set_output_style`、`seed_read_state` | A7：busy 检查、permission registry、transcript 写锁、SDK config、下一轮生效；依赖 A6 项目 generation | `B/src/gateway/client/InProcessGateway.ts:2191-2209,2321-2330,2364-2385,2423-2442`、`B/src/cli/createLocalGateway.ts:1403-1414` |
| S6 · 运行时工具与统计（6） | `mcp_server_status`、`usage_snapshot`、`model_usage_snapshot`、`rewind_files`、`background_task_stop`、`background_tasks` | A7：A6 的 model/MCP/file history/background/subagent provider；不返回占位空列表冒充支持 | `B/src/gateway/client/InProcessGateway.ts:2279-2282,2392-2421`、`B/src/cli/ProjectRuntimeRegistry.ts:1293-1347,1707-1751` |
| S7 · 自动化会话（5） | `sop_status`、`sop_resume`、`always_on_apply`、`always_on_abort`、`always_on_rerun_plan` | A7：接到同一 submit/admission/abort，不能为自动化另起隐形 TS 会话 owner | `B/src/gateway/client/InProcessGateway.ts:634-652,2992-3030`、`B/src/cli/createLocalGateway.ts:631-642,1072-1080` |
| S8 · run 读面（3） | `run_get`、`run_events`、`run_reattach` | A7：A6 持久 run registry 的 ownerPid、revision、seq、重复接纳与孤儿恢复；read 不只查缓存 | `B/src/gateway/client/InProcessGateway.ts:2525-2553`、`B/src/gateway/run/RunRegistry.ts:73-102,211-282,285-307` |

上述“全覆盖”只指50个method均有工作包，**不表示所有底层业务分支已调研完**。例如SOP还分 approverAuthorization/human/source 路径，拒绝客户端覆盖authority/subject；迁移时必须保留这些边界，A5.1再向实际provider展开，不把五个自动化method估成五个简单转发函数。证据：`B/src/cli/createLocalGateway.ts:1072-1089`、`B/src/gateway/client/InProcessGateway.ts:2992-3030`。

另有一个必须保留的**静态竞态疑点**：旧socket的 `disconnectInteraction` 若因binding已换而返回false，`preserveTurn`也为false，而连接close仍可能按sessionKey、不带runId调用abort。不能笼统断言“TS旧连接close绝不会abort新owner”。A5以受控旧close/新reconnect交错定标；A7对显式runId的旧abort按已有fence实现，对无runId的连接close不擅自修行为，若证实基线bug再单独登记。证据：`B/src/interaction/InteractionReconnect.ts:189-196`、`B/src/gateway/client/GatewayInteractionCoordinator.ts:86-94`、`B/src/gateway/server/GatewayWsConnection.ts:48-58`、`B/src/gateway/client/InProcessGateway.ts:2000-2013`。

## 三、待定41：逐组给去向，不把“不知道”当成无状态

表内 41 个仍是原冻结“待定”；本轮只是确认依赖与计划归属。**A6 构建底层 provider，A7 消费会话能力，A8 完成剩余 method/HTTP 入口和统一切换**。A6/A7 期间原默认链路继续透传；独立 Rust 验证使用隔离 home/project。不能在同一配置作用域把下表写操作留 TS 而开放 Rust 会话写者（依据：`B/src/cli/GatewayRuntimeRefreshBundle.ts:149-160`）。

| 包 | method（数量） | 本次确认 / 尚缺 / 计划去向 | 证据 |
|---|---|---|---|
| U1 上传（5） | `upload_create`、`upload_get`、`upload_part`、`upload_complete`、`upload_cancel` | native 默认是 UploadStore：盘上 manifest/文件 + 进程内串行队列；get 可能触发过期处理。submit 持 attachment lease。A6 实现 provider，A7 验附件轮次，A8 暴露全部入口；外部 uploadLifecycle 分支仍需夹具。 | `B/src/cli/GatewayDialogBundle.ts:66-72`、`B/src/gateway/dialog/UploadStore.ts:48-64,98-121,189-211,282-290`、`B/src/cli/GatewayUploadedAttachmentBundle.ts:28-66` |
| U2 memory（2） | `memory_list`、`memory_wipe` | 经 registry.memoryManagement；ProjectMemoryBundle 可选 native 或注入 provider，有显式 dispose。**未核 EdgeClaw 包内真实存储与调度**。A6 provider 清单先补证，A8 交付 management API；不能预判纯文件读写。 | `B/src/cli/createLocalGateway.ts:1238-1259`、`B/src/cli/ProjectMemoryBundle.ts:49-85` |
| U3 snapshot（3） | `snapshot_list`、`snapshot_get`、`snapshot_restore` | 可选 workspaceSnapshotProvider，list/get 校验 workspace，restore 创建目标临时目录，submit 还有 pre/post recorder。**provider 内存/盘/外部配置覆盖未完**。A6 迁 recorder/provider，A8 迁管理入口。 | `B/src/cli/createLocalGateway.ts:615-620,1271-1313`、`B/src/gateway/client/InProcessGateway.ts:1613-1625,1827-1839` |
| U4 manager（2） | `manager_sessions`、`manager_browsers` | 前者走 catalog consumer，不可简化为 Router Map；后者直接注入可选 port。**浏览器 provider 的部署绑定未核**。A6 catalog/provider，A8 method；既有未配置错误可对齐，但不得通过不接 provider 使成功路径永久不可用。 | `B/src/cli/createLocalGateway.ts:1261-1270`、`B/src/cli/ProjectRuntimeRegistry.ts:1703-1705`、`B/src/gateway/client/InProcessGateway.ts:2832-2835` |
| U5 commands（1） | `commands_list` | refresh/acquireCommandCatalog/release 绑定 plugin generation。A6 与 extension runtime 一起迁，A8 入口验收。 | `B/src/cli/GatewayCommandCatalogBundle.ts:46-58` |
| U6 MCP（4） | `set_mcp_servers`、`mcp_server_reconnect`、`mcp_server_toggle`、`set_mcp_permission_mode_override` | IPG 委派 project/runtime port，必须同 S6 的 mcp status/provider。A6 持有连接与配置，A8 迁写/重连入口并验失效。 | `B/src/gateway/client/InProcessGateway.ts:2284-2320`、`B/src/cli/ProjectRuntimeRegistry.ts:1293-1310` |
| U7 全局配置（6） | `apply_flag_settings`、`update_settings`、`resolve_settings`、`reload_output_styles`、`reload_config`、`reload_extensions` | configStore 内存快照、runtime reload、dirty 与广播是同一链。A6 先建设配置/租约 owner，A8 全部切入该 owner；只改 YAML 不算完成。 | `B/src/gateway/client/InProcessGateway.ts:2337-2363,2387-2390,2905-2910,2923-2928`、`B/src/cli/createLocalGateway.ts:1402,1490-1494`、`B/src/cli/GatewayRuntimeRefreshBundle.ts:71-111,134-163` |
| U8 cron（6） | `cron_create`、`cron_list`、`cron_update`、`cron_delete`、`cron_stop`、`cron_run_now` | CronManager 有 runtimes Map 与 start/stop；不是只迁 JSON 文件。A8 迁 scheduler + 管理入口，接到 A7 会话接纳；切换时旧 scheduler 也要停。完整触发/重启语义仍需展开。 | `B/src/cron/runtime/CronManager.ts:52,82-111,115-176`、`B/src/gateway/client/InProcessGateway.ts:2603-2625` |
| U9 async hook（1） | `hook_async_result` | pendingAsyncHooks/outcomes、expiresAt、重复提交、steer 投递与 turnId 耦合，属于会话运行时依赖。A7 与 hook 生产者同迁，A8 核总表。 | `B/src/gateway/client/InProcessGateway.ts:694-729,1896-1997` |
| U10 subagent history（1） | `read_subagent_messages` | history provider 先 recoverTranscriptReplacements，可能写盘；必须与 A6 存储/A7 replacement 同 owner。A7 实现，A8 核总表。 | `B/src/cli/GatewaySessionHistoryBundle.ts:137-147` |
| U11 skills（8） | `skill_list`、`skill_read`、`skill_write`、`skill_create`、`skill_delete`、`skill_import`、`skill_validate`、`skill_scan` | 先读 runtime config 选 disabled/external/native，四个读方法也不等于直接扫盘。A6 持 generation 与 provider，A8 全入口；需覆盖三种 binding 的既有语义。 | `B/src/cli/createLocalGateway.ts:851-870`、`B/src/gateway/client/InProcessGateway.ts:2950-2980` |
| U12 channel（1） | `prepare_weixin_login` | CLI 在 server 创建后注入回调，实际调用 hotStartWeixinChannel；删 TS CLI 前必须补此 wiring。A8 channel 生命周期与 HTTP webhook 一起做；不能用固定 unsupported 隐去已启用功能。 | `B/src/gateway/client/InProcessGateway.ts:2912-2921,2938-2939`、`B/src/cli/pilotdeck.ts:233-247` |
| U13 能力声明（1） | `describe_server` | 能力由 options/provider 是否存在决定，hello_ok 同源。A7 原生隔离验证时生成真实声明，A8 全配置验收；默认代理期仍透传 TS 的 hello。 | `B/src/gateway/client/InProcessGateway.ts:2088-2117`、`B/src/gateway/server/GatewayWsConnection.ts:110-118,361-362` |

**不确定处的关闭方式**：U2/U3/U4 的 A6 首个 PR 先补“配置入口 → 实际 provider → 存储 → 取消/释放”逐行证据和成功/未配置/失败夹具；没补完就不声称该 provider 已 Rust 化。外部配置分支不是默认可删功能，范围若要收窄走 D6。依据：`B/src/cli/ProjectMemoryBundle.ts:57-78`、`B/src/cli/createLocalGateway.ts:615-620,1270`、`.42cog/intent.md:23,27`。

## 四、#19：宿主竞态的可定位部分与不可冒认部分

### 4.1 状态来源

2026-10-14 读取 [#19 最新状态评论](https://github.com/zhengdaqi/pilotdeck-rs/issues/19#issuecomment-6160592863)：PR #48 已补 aborted turn_completed、lastAppliedSequence 对齐，两条 deadline 各 4/5；剩余差异挂阶段 A，issue 保持打开。**这是引用协调者已有结果，本轮未重跑**。本仓可审计依据仍是 `specs/parity/accepted-diffs.md:9-11`：前两条曾归“原版缺陷”，第 3 条更正为“Rust 产品问题”，全部待批；最新在线交接不等于自动撤销该更正或批准豁免。

### 4.2 因果链（源码确认）

| 步 | 发生什么 | 证据 |
|---|---|---|
| 1 | gateway 从 timeoutMs 派生 operationDeadline，同时设本地 timer；timer 先置 timedOut，再拒绝 pending、abort session、等待写 timeout status，随后发布 status/error 并关 queue。 | `B/src/gateway/client/InProcessGateway.ts:1384-1439` |
| 2 | sidecar client 遇 Module final=result_unknown，先记录 unknown，再查 operation ledger/外部 reconciler；查不到抛 AgentLoopResultUnknownError。普通 turn_completed 被抑制，不可用于解 unknown。 | `B/src/agent/modules/transport/agentLoopSidecarClient.ts:662-684,1017-1033` |
| 3 | ledger 只接受同 identity/stream 的已知 terminal 作为对账结果；写 unknown 不会凭空创造 known。 | `B/src/agent/modules/transport/sessionOperationLedger.ts:132-164` |
| 4 | TurnRunner 对精确 `timeout:<runId>` 的 aborted signal + unknown 直接重抛，让 gateway timeout 路径收尾；其余错误走正常失败投影。 | `B/src/agent/turn/TurnRunner.ts:422-440,701-704` |
| 5 | gateway catch 的 `!timedOut` 只保护 postFailure 字段；写 gateway_submit_failed 的分支检查的是 completion handle 当前性，**没有检查 timedOut**。写状态本身有 await。 | `B/src/gateway/client/InProcessGateway.ts:1708-1760`，状态写入/发布见 `:1252-1266` |
| 6 | **运行期** completion handle 只在外层 finally 的 complete（:1848）移除；timeout writer 返回或 queue.close 都不移除 handle。:984 的 releasePreflight 是接纳前释放路径，不影响这里的 #19 运行期交错。catch 在 handle 仍 current 时先写 durable status，再尝试发布；queue.close 不会撤销此前的状态落盘，帧与 registry 的后续行为还须看第 7–8 步。 | `B/src/gateway/client/InProcessGateway.ts:974-986,1252-1266,1745-1759,1774-1849`、`B/src/gateway/client/GatewayTurnCompletionFence.ts:26-41` |
| 7 | **无 runRegistry** 时直接 queue.enqueue；**有 runRegistry** 时每帧先异步 append，成功后的 .then 才 enqueue。timeout 路径调用 enqueueGatewayEvent 后紧接同步 queue.close，因此 timeout 自己的帧也可能在 append 完成前被关队列丢弃。生产 createLocalGateway 总会注入 registry，默认 FileRunRegistry；不能用无 registry 的夹具代表生产。 | `B/src/cli/createLocalGateway.ts:996`、`B/src/gateway/client/InProcessGateway.ts:1216-1229,1420-1434`、`B/src/gateway/util/AsyncQueue.ts:17-18` |
| 8 | FileRunRegistry 的终态拒绝条件不含 failed；timeout error 可将 state 设为 failed，后到的 gateway_submit_failed error 仍可追加到 events 并覆盖 record.result。queue.close 不阻止 append，实时帧不可见不表示 registry events/result 未变；最终顺序和故障落盘结果待复现。 | `B/src/gateway/run/RunRegistry.ts:236-260` |

**源码支持的机制与待复现边界**：timeout-first 后 unknown 进入同一 current handle 的 catch 时，成功写入的 durable status 可追加 gateway_submit_failed；handle 的生命周期使 fence 仍通过，不能把慢 writer 当作其原因。**有 / 无 runRegistry 是独立维度**：无 registry 时直接入队，关闭后丢后续帧；生产有 registry 时 append 与入队之间存在异步边界，timeout 自己的帧也可能丢失，迟到失败还可能改变 registry events 并覆盖 result。因此“queue 关闭只隐藏帧”不能概括生产路径，也不能从帧只有 timeout 推断 durable status 或 registry 只有 timeout。

**尚未实测**：两种 registry 拓扑 × 快/慢/失败 writer 下实际帧、durable status、registry events/result 的组合、顺序与故障结果；这些须在 A5.2 分开断言、实测复现，不把源码推导写成固定输出。也尚未证明所有 #19 差异都来自这一交错，或 unknown-first、timer-first 在真实调度下各自比例；unknown 迟至外层 complete 后的 current 检查另测，不能与 writer 快慢混为一谈。依据为上表，复现与 bug 归类仍交 A5/#19。

### 4.3 Rust 宿主建议：两条状态轴，一处裁决

建议保持两个正交状态轴（依据：Module unknown fail-closed 与 gateway timeout 各有独立职责，`B/src/agent/modules/transport/agentLoopSidecarClient.ts:1017-1033`、`B/src/gateway/client/InProcessGateway.ts:1384-1439`）：

1. **执行事实轴**：started → accepted → known terminal 或 unknown；只有同 run/operation/request/stream/module binding 的已知 durable 结果可解 unknown。普通 Completed、迟到 ACK、重复 callback 均不能授权重放副作用。对齐 `B/src/agent/modules/transport/sessionOperationLedger.ts:52-164`、`B/src/agent/modules/transport/agentLoopSidecarClient.ts:1040-1075,1080-1151`。
2. **可见收尾轴**：open → settling(reason, run/handle) → settled。每会话单一裁决器先原子保留终态，再发 abort/等待持久化；等待 I/O 不让 sibling catch 获得第二个可见终态写权。timeout 到期立即停止推进，写盘失败仍保持 timeout 错误；不标成功。对齐既有断言 `B/tests/gateway/operation-deadline.spec.ts:117-160,163-197`，修正范围由 D4 决定。

| 交错/故障 | 建议 Rust 处理与应有断言 | 与 TS 的关系 |
|---|---|---|
| timeout 已取得收尾权，unknown 后到；有 / 无 registry × 快/慢/失败 writer | 当场 abort；unknown 照记执行账本；成功持久化分支的候选断言为：帧仅一套 timeout status/error，durable status 不追加 gateway_submit_failed；有 registry 时另断言 events 不追加迟到失败、result 保持 timeout，不被覆盖，无 registry 时该项明确不适用；队列关闭须等待应持久化并发布的帧，失败分支另定标，不伪称写入成功 | **候选修 bug②，非基线实测结果**；需批准帧、durable status、registry events/result 的具体差异规则；依据 `B/tests/gateway/operation-deadline.spec.ts:154-160`、`B/src/gateway/client/InProcessGateway.ts:1216-1266,1433-1434,1727-1759`、`B/src/gateway/run/RunRegistry.ts:242-258` |
| unknown 在本地 timeout 生效前到达 | 保持 fail-closed 与原失败投影；仅凭 code=DEADLINE_EXCEEDED 不能认定 gateway timeout 已成立；若要统一该交错，先补基线证据再扩 D4 | `B/src/agent/turn/TurnRunner.ts:424-439,701-704`；不预先扩大 bug 结论 |
| 已知 final 与 timeout 同时到达 | 以拥有者串行接收/校验后的首次有效裁决为准，测试两种确定顺序；不会两个可见终态都赢 | **建议，需 A5 定标**；依据 TS 延后终态与 timeout 并行路径 `B/src/gateway/client/InProcessGateway.ts:1695-1699,1779-1790` |
| 请求取消、工具已执行未回 ACK、sidecar 退出/重启 | 分开 Requested 与 Deadline；未确认 effect 保持 unknown；不自动重新 execute；旧 generation 回复只记诊断，不改新 run | `B/src/agent/modules/transport/agentLoopSidecarClient.ts:1000-1014,1040-1055,1123-1151`、`B/src/gateway/client/GatewayTurnCompletionFence.ts:26-41` |
| timeout status 写失败 / run registry 写失败 | 候选目标：前者仍发布 timeout error，后者暴露持久化不确定性，均不转 success；生产 queue.close 与异步 append 可能使错误帧也不可见，基线帧及实际落盘须另行复现，不能写成既有保证 | `B/tests/gateway/operation-deadline.spec.ts:163-197`、`B/src/gateway/client/InProcessGateway.ts:1216-1243,1433-1434,1798-1809` |

**bug 候选登记 A19-HOST（待定标、待批准，非 accepted diff）**：拟修正的是“gateway timeout 已拥有收尾权时，迟到 reconciliation failure 仍追加 operational failure”。类别建议为 intent bug②；证据为 catch 缺守卫与测试断言。A5 应把有/无 registry 的受控重现、帧/durable status/registry events 与 result 差异、原/新断言回填现有 #19；A7 实现 Rust 收尾后，在获批的 `specs/gateway-parity/accepted-diffs.md` 追加精确条目。若影响阶段 C 评分，另按阶段 C 清单流程登记，不能由 A 的结论倒算 C 通过。本轮不写任何 specs，不新造“原版缺陷已批准”结论。依据：`.42cog/intent.md:23`、`specs/parity/accepted-diffs.md:3-11`、`CLAUDE.md:115-128`。

## 五、按阻断点安排验证

| 缺口 | 等级/阻断位置 | 必须取得的证据 | 依据 |
|---|---|---|---|
| 宿主 provider 尚未移植 | P1 / A6 完成、A7 原生域 | model/tool/context/permission/lifecycle/event/turn/budget 各有真实 host contract 测试；fixture mock 只充当外部模型/工具，不冒充 Rust 宿主实现 | `B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:394-437` |
| 50 methods 状态闭包未验证 | P1 / A7 完成 | 双客户端争权、SDK 控制权限、重复/过期答复、disconnect→reconnect→answer→终态、close/delete 与在飞写互斥、history epoch/替换恢复、run orphan | 本文第二节每包的直接证据；`B/src/interaction/InteractionReconnect.ts:105-136` |
| 全局写与旁路入口仍能触及 TS | P0 / A8 原生切换 | 41 项逐一明确实现/既有可选不可用分支；HTTP/channel/cron 与 WS 共享 Rust owner；无 TS gateway 的进程与出站连接证据 | `B/src/cli/GatewayRuntimeRefreshBundle.ts:134-163`、`B/src/gateway/server/GatewayServer.ts:96-131`、`B/src/cli/pilotdeck.ts:233-249` |
| 当前装置不支持原生拓扑 | P1 / 新宿主验收 | 运行器增加明确 topology/sidecar 选择；native 轮不得启动 TS gateway，加入“偷偷回退到 TS”阳性失败对照 | `scripts/gateway-parity/README.md:35-39`、`scripts/gateway-parity/run_round.py:221-230` |
| 真实 UI 自动恢复缺陷 | P0 / 完整 UI 恢复声明 | D5 拍板并有真实 bridge 录帧；不可用协议探针替代这道门 | `docs/experiments/exp003-transparent-proxy/readme.md:55,65-70` |
| #19 未完成严格归类与受控复现 | P1 / #19 关闭和 A7 超时验收 | 模型等待/工具等待 × timeout-first/unknown-first × 有/无 registry × 快/慢/失败 writer；实测复现后分别断言帧、durable status、registry events/result，另测外层 complete 后的 fence 边界，不预判结果 | `B/src/cli/createLocalGateway.ts:996`、`B/src/gateway/client/InProcessGateway.ts:1216-1266,1384-1439,1708-1760,1848`、`B/src/gateway/run/RunRegistry.ts:242-258` |

阶段 A 的 TS gateway 退役不等于删整个 TS 仓或 Node：原版 ui/server 仍按阶段边界保留，桌面仍有自己的启动路径；验收应检查被替换的 **gateway/宿主进程及内部回调**，而非禁止 TS 测试驱动或前端进程。依据：`.42cog/intent.md:24,42`、`B/apps/desktop/src/main.ts:250-258`。
