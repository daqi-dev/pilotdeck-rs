---
type: research
number: "008"
date: 2026-10-14
title: 阶段 A 深水区：会话域的最小完整迁移单元
tags: [phase-a, session, interaction, sidecar]
status: 源码调研完成；运行验证与决策闸门待后续 issue
related: [research/005_phase-a-gateway, research/008_phase-a-session-domain/gap, research/008_phase-a-session-domain/decision, plan/005_phase-a-session-domain]
---

# 008 · 必须先知道的事

**建议把“构建粒度”和“切换粒度”分开：按状态所有者分小 PR 构建，连接、会话、交互及其宿主依赖准备齐后，在停止接收新轮次并排空旧运行时的边界整块切换；不要逐个打开 (b)(c) method。** 依据是连接注入 binding、会话接纳锁、交互 bus 与收尾持久化相互依赖：`B/src/gateway/server/GatewayWsConnection.ts:123-169`、`B/src/gateway/SessionRouter.ts:217-249`、`B/src/gateway/client/GatewayInteractionCoordinator.ts:75-125`、`B/src/gateway/client/InProcessGateway.ts:1779-1849`。方案与取舍见 [decision.md](decision.md)，事实缺口见 [gap.md](gap.md)，实施顺序见 [计划 005](../../plan/005_phase-a-session-domain.md)。

## 证据口径

- 本目录 **`B/` 是只读基线根 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/`**，固定 commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；无 `B/` 前缀的路径相对本仓。本次只读源码和已有实验，不新增运行结论。固定基线原则见 `.42cog/intent.md:9-11,30-37`。
- **事实**来自路径行号；**推导/建议**写明依据而不冒充实现；**未证实**进入 gap，不据此放行。旧报告中的实测只按其明确边界引用：`docs/experiments/exp003-transparent-proxy/readme.md:47-70`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:74-78`。
- 分母仍为 **101 = (a)10 + (b)46 + (c)4 + 待定41**，不修改原冻结分类。`hello` 是帧，非第 102 个 method。计划 002 / 研究 005 旧摘要中的 15、36 与“另开计划 003”不再作为本轮依据；003 已用于阶段 B，本轮承接文件是 005。证据：`docs/research/005_phase-a-gateway/method-inventory.md:13,127-135`、`docs/plan/002_phase-a-gateway.md:96-98`、`docs/plan/README.md:13`。

## 不能拆开的状态边界

| 边界 | 已确认事实 | 对迁移的约束（推导） |
|---|---|---|
| 连接 → 交互 | 连接生成 UUID + 进程 generation，经 hello_ok 下发；服务端覆盖 submit/elicitation/reconnect/permission 的 binding，SDK 独立控制连接有权限豁免。`B/src/gateway/server/GatewayWsConnection.ts:8-38,110-118,133-136,450-505` | Rust 接管 hello 时必须同时有交互权威；不得把 Rust binding 塞给仍持有 TS bus 的上游。SDK 控制连接也不能误加普通 Web 连接约束。 |
| 交互 → 等待中的工具 | reconnect provider 同时持有 pending/current/last 三张表；重连更新 pending binding，consume 校验并删除；bus entry 带等待方回调。`B/src/interaction/InteractionReconnect.ts:67-70,105-136,182-196`、`B/src/gateway/permission/GatewayPermissionBus.ts:53-110`、`B/src/gateway/client/GatewayInteractionCoordinator.ts:108-125` | 搬记录而不搬等待方没有意义。permission/elicitation 的生产者、消费、取消、断线和超时清理必须由同一宿主完成。 |
| 连接断开 → 轮次 | 断开遍历 in-flight 与 interaction sessions；pending permission/elicitation 时 preserveTurn，否则在飞轮次 abort。`B/src/gateway/server/GatewayWsConnection.ts:48-63`、`B/src/gateway/client/GatewayInteractionCoordinator.ts:86-100` | 旧连接 close、重连、答复、timeout 必须按会话顺序判定。不能把“socket 断开”一律等同“轮次取消”，也不能把 user_dialog 自动加进 preserveTurn 判据。 |
| 接纳 → 收尾 | Router 持有 inFlightTurns / maintenance；completion fence 只检查当前 handle 与通知 drain，不负责接纳。`B/src/gateway/SessionRouter.ts:78-90,217-249`、`B/src/gateway/client/GatewayTurnCompletionFence.ts:15-41`、`B/src/gateway/client/InProcessGateway.ts:1845-1849` | 一个会话的 submit、steer、abort、close、replacement、配置变更必须使用同一接纳权威；旧 run 的迟到回调不能结束新 run。 |
| 历史 → 在飞视图 | read_session_messages 读历史前后核对 epoch，至多三次；replay 为内存有界窗口，默认 500 events / 256KiB / 终态 30 秒。`B/src/gateway/client/InProcessGateway.ts:2847-2860`、`B/src/gateway/client/GatewayTurnReplayStore.ts:8-10,48-88,107-120` | 读盘不能替代实时快照；replay 不能替代 durable transcript / run registry。三种序号（WS 请求 seq、run event seq、Module stream sequence）分别保持原语义。WS seq 见 `B/src/gateway/server/GatewayWsConnection.ts:129-169`，run seq 见 `B/src/gateway/run/RunRegistry.ts:236-250`。 |
| user_dialog → 存储与 renderer 租约 | 内存 bus 的 lease 可选，但有 lease 时必须匹配；hosted answer 优先于内存 bus，随后才是 recovered 分支。文件 store 有独立 owner/renderer 租约与 session 锁。`B/src/gateway/user-dialog/GatewayUserDialogBus.ts:113-133`、`B/src/gateway/client/InProcessGateway.ts:2658-2707`、`B/src/gateway/user-dialog/FileGatewayUserDialogStore.ts:70-73,149-171,227-245` | user_dialog 不是 permission bus 的换名。需要把 live/hosted/recovered、租约续期/过期、答复验证及 journal 清理一起迁移；不得用 WS binding 代替 bearer lease。 |
| 会话 → 项目运行时 | config reload 驱动 registry.reload → markAllDirty → 广播；commands 读取先 refresh 并持有 plugin generation lease。`B/src/cli/GatewayRuntimeRefreshBundle.ts:134-163`、`B/src/cli/GatewayCommandCatalogBundle.ts:46-58` | 同一项目中保留 TS 全局写者却让 Rust 独立缓存配置，会产生两个权威。全局状态依赖是切换前置，不能把 41 个待定统称“以后透传”。 |

## 状态权威从 TS 搬到 Rust 的建议

这是待实施的所有权设计，**不主张复制 TS 进程内存**。基线可恢复物是 transcript operation ledger、run registry、配置/历史和可选 dialog store；binding、等待回调、replay timer 都是进程内对象。依据：`B/src/agent/modules/transport/sessionOperationLedger.ts:28-38,193-237`、`B/src/gateway/run/RunRegistry.ts:158-193,285-307`、`B/src/interaction/InteractionReconnect.ts:67-70`、`B/src/gateway/user-dialog/GatewayUserDialogBus.ts:78-79`。

| 建议 Rust 所有者（逻辑模块，非强制新 crate） | 持有什么 | 恢复和退役边界 | 基线依据 |
|---|---|---|---|
| ConnectionContext | auth/clientName、binding、in-flight/interaction session 集合、每请求 WS seq | 新连接产生新 binding；进程重启不伪造旧 pending 回调 | `B/src/gateway/server/GatewayWsConnection.ts:29-63,95-169` |
| SessionRuntime | Router 接纳、runId、completion handle、steer mailbox、pending interaction、permission mode/grants、replacement reservations、emit sink | 每会话串行裁决；await 外部 I/O 前保存 run/handle，回调回来重验身份；不跨 I/O 持有全局锁 | `B/src/gateway/SessionRouter.ts:217-249,307-337`、`B/src/gateway/client/GatewayTurnReplacementCoordinator.ts:42-75,131-177`、`B/src/gateway/client/InProcessGateway.ts:1587-1612` |
| DurableSessionStore | transcript、operation started/accepted/unknown/known terminal、history replacement、run registry | 先写成功再发布对应 durable 事实；unknown 与已知终态分开；重启按 owner 存活/事务恢复处理 | `B/src/agent/modules/transport/sessionOperationLedger.ts:52-164`、`B/src/gateway/client/InProcessGateway.ts:1787-1809`、`B/src/gateway/run/RunRegistry.ts:285-307` |
| ProjectRuntime | config generation、provider/plugin、model/tool/context、MCP、后台任务、memory/snapshot、cron/SOP/always-on 接纳 | 一个配置作用域一个权威；退休 runtime 必须排空 lease/任务，广播由同一 owner 发出 | `B/src/cli/GatewayRuntimeRefreshBundle.ts:84-111,134-163`、`B/src/cli/ProjectRuntimeRegistry.ts:1707-1751`、`B/src/cron/runtime/CronManager.ts:52,105-111` |
| Dialog/Upload provider | live dialog 与 renderer/owner lease；上传 manifest、artifact lease、retention | 原 provider 的锁/租约/磁盘格式边界分别保留，不归并为一个 session Map | `B/src/gateway/user-dialog/FileGatewayUserDialogStore.ts:70-73`、`B/src/gateway/dialog/UploadStore.ts:48-64,98-121,282-290` |

建议切换顺序为：独立 home/project 验证 Rust → 停止旧侧所有入口的新接纳（WS、HTTP、cron、channels）→ 完成或显式中止旧轮次并排空持久化、租约和后台 writer → 停止旧 owner → Rust 从磁盘恢复 → 新连接握手。任何 drain/恢复失败都保留旧证据并停止这一切换，不把未完成轮次移到新侧；回滚也要先排空 Rust，再让 TS 恢复经验证兼容的数据。依据是 Router 的 shutdown/close-project 防创建、run orphan 检查和上传 lease：`B/src/gateway/SessionRouter.ts:110-127,182-211`、`B/src/gateway/run/RunRegistry.ts:285-307`、`B/src/gateway/dialog/UploadStore.ts:282-290`；选型见 decision D1/D3。

## 与阶段 C 的关系

**推荐 Rust gateway 直接驱动阶段 C Rust sidecar，继续用已有 Module Protocol；同时必须实现 Rust 宿主。** 基线 stdio 连接工厂只负责 spawn / JSONL；model、工具执行、权限、context、lifecycle、event 与持久化仍在宿主。Rust 现有 transport 的 `BoundExchange` 也会向外发送 `module_call`，不是这些生产能力的实现。依据：`B/src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts:26-31,74-97`、`B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:394-437`、`src/pilotdeck-transport/src/stdio.rs:426-438,469-485`。

建议用四格对照隔离错误：TS host + TS sidecar（固定基准）；TS host + Rust sidecar（既有 C）；Rust host + TS sidecar（测新宿主）；Rust host + Rust sidecar（最终集成）。阶段 C 的 53 场景语义尺子与 A 的帧/落盘/HTTP 尺子分别保留；新宿主接到同一语义驱动需要新装置入口，不能把当前 `--side rust` 当成已支持无 TS host。证据：`.42cog/intent.md:9-11`、`scripts/parity/round.py:132-157`、`scripts/gateway-parity/run_round.py:221-230`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:74-78`。进程内链接可作为迁移后的独立优化；本轮先保留现有进程边界，以便用四格对照区分宿主与 sidecar 的错误归属，这是 [decision D2](decision.md) 的技术建议，不是意向书对双拓扑的明文禁令。

## #19 与“不确定结果”的底线

**外部副作用是否已知**与**gateway 是否已向用户发布终态**是两个维度。sidecar 的普通 aborted `turn_completed` 不是可信 Module 最终结果；TS 客户端明确压住它，只接受 final 或宿主对账所得终态。无已知 ledger 结果时抛 `AgentLoopResultUnknownError`。依据：`B/src/agent/modules/transport/agentLoopSidecarClient.ts:662-684,1017-1033`、`B/src/agent/modules/transport/sessionOperationLedger.ts:132-164`。

因此 Rust 宿主必须保留 unknown 责任、阻止未经确认的副作用重发，同时让超时收尾只有一个可见裁决。**不能为消除 `gateway_submit_failed` 就把 unknown 改 success、吞所有异常或改比较器。** timeout 与错误如何仲裁、哪些行为属于基线 bug，分别见 gap 的 #19 因果链和 decision D4；规则依据为 `.42cog/intent.md:23,27,34-36`、`B/src/gateway/client/InProcessGateway.ts:1384-1439,1708-1760`。
