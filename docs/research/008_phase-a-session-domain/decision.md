---
type: research
number: "008"
date: 2026-10-14
title: 阶段 A 深水区：架构与验收决策卡
tags: [phase-a, decision, session, deadline]
status: 技术方案形成建议；D4/D5 等人；本轮未发车或改尺子
related: [research/008_phase-a-session-domain/must-know, research/008_phase-a-session-domain/gap, plan/005_phase-a-session-domain]
---

# 008 · 决策卡

**建议：保留进程式 sidecar 边界，建设 Rust 宿主；按所有者分小 PR，按完整运行时切换；#19 用两条状态轴处理，修 bug 的行为差异须单独定标。** 依据：`B/src/gateway/server/GatewayWsConnection.ts:123-169`、`B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:394-437`、`B/src/gateway/client/InProcessGateway.ts:1384-1439,1708-1760`。`B/` 为 [must-know.md](must-know.md) 定义的只读基线根。

以下按 `CLAUDE.md:115-128` 的三档填写。**“自决·待复核”只表示本计划采用该技术建议（AI 代批，待人复核），不表示已经实现、已经发车或有新的用户批准。** 本任务只许写指定文档，决策卡暂存在此；发车后由协调者按 `CLAUDE.md:121-128` 登记到当日 state 决策文件，本轮不写 state。D4/D5 只阻断对应行为/验收线，调研、兼容实现和离线构建可以继续。

## D1 · 切换的最小单元

| 卡项 | 内容 |
|---|---|
| 卡在哪 | (b)(c) 有共同 binding/bus/Router，41 个待定又包含配置失效、上传 lease 与 cron；单独放开 submit 或 permission handler 会把状态拆到两侧。依据：`B/src/gateway/client/GatewayInteractionCoordinator.ts:75-125`、`B/src/cli/GatewayRuntimeRefreshBundle.ts:149-160`、`B/src/cli/GatewayUploadedAttachmentBundle.ts:34-66`。 |
| 谁受影响 | 所有原版 gateway 客户端、在飞轮次、HTTP/cron/channel 入口。HTTP 与 WS 接同 gateway，证据 `B/src/gateway/server/GatewayServer.ts:96-125`；cron 内存生命周期见 `B/src/cron/runtime/CronManager.ts:52,105-111`。 |
| 能否撤回 | 开发阶段可以：默认代理路由保留，原生测试独立数据目录。已切换并写入真实数据后不能无条件回滚，必须先排空并核存储兼容，依据 `B/src/gateway/run/RunRegistry.ts:184-211`。 |
| 动不动尺子 / 意向 | 不改协议与意向；新增拓扑与覆盖会触及装置登记，走 A5 审核，不能当作当前尺子已有能力。依据 `specs/gateway-parity/FROZEN.md:3-6`。 |
| 选项与后果 | A 按 method：需跨侧同步 binding/等待方/配置，扩大协议与双写风险。B 按 session 分流：需要一个真正的统一 binding/广播/全局配置 owner，本轮无此实现证据。C **同一数据/配置作用域整体运行时切换**：实现可渐进，最终有排空窗口，避免热迁等待回调。 |
| 建议与分级 | **选 C，自决·待复核**。A5–A7 构建；A8 一次切换所有生产入口。可在独立 home/project 的隔离实例上提前测试完整 Rust 会话，不能同盘混合两个运行时 authority。 |
| 原则 → 断言 | 一个状态域只有一个可写权威 → native 轮 process manifest 不含 TS gateway；注入一次回退或双 owner，装置必须 FAIL；旧运行时未 drain 时切换失败。依据 `B/src/gateway/SessionRouter.ts:78-90,110-127,203-211`。 |

## D2 · Rust gateway 如何驱动阶段 C

| 卡项 | 内容 |
|---|---|
| 卡在哪 | C 是 agent loop，host 能力仍向外请求；直接链接 agent crate 并不能补齐 model/tools/context/persistence。证据 `src/pilotdeck-transport/src/stdio.rs:469-485`、`B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:394-437`。 |
| 谁受影响 | C 的 transport/replay/cancel 语义、A 的 session host、子进程清理。基线 stdio spawn 与 send/receive 在 `B/src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts:74-105`。 |
| 能否撤回 | 可以切回已验证的进程拓扑；进程内化另开后续优化，不能在未量等价前替代。依据 `.42cog/intent.md:26`。 |
| 动不动尺子 / 意向 | 保留 C 冻结协议/尺子；新增 Rust-host 对照入口，按 A5 登记。阶段 A 与 C 的归因仍分开，依据 `.42cog/intent.md:9-11,24`。 |
| 选项与后果 | A 进程内链接：减少运输，但重做取消/故障隔离/回放验证。B **Rust host → Module Protocol → Rust sidecar**：保留可量边界和故障域，新增 host client/dispatcher 与生产 provider。C 留 TS host-helper：可作诊断对照，不能满足 TS 宿主退役。 |
| 建议与分级 | **选 B，自决·待复核**。A6 用 TS/Rust 两种 sidecar 交叉量宿主；最终 Rust→Rust。复用现有 protocol/agent/transport 的契约与适用实现，不把 sidecar 内的 `BoundExchange` 当成完整 gateway client。 |
| 原则 → 断言 | 改一个执行边界，保留另一个对照 → 四格矩阵各自落报告，启动 argv/二进制 hash/握手 module identity 必须对应声明；声称 native 时 TS host-helper 必须被拒绝。依据 `B/src/agent/modules/transport/agentLoopSidecarClient.ts:1040-1055`。 |

## D3 · 内存权威迁移与重启承诺

| 卡项 | 内容 |
|---|---|
| 卡在哪 | binding/permission/elicitation 的 Map 内有回调，不能靠拷贝 transcript 恢复。dialog 另有 hosted/recovered 协议。证据 `B/src/gateway/permission/GatewayPermissionBus.ts:53-110`、`B/src/interaction/InteractionReconnect.ts:67-70`、`B/src/gateway/client/InProcessGateway.ts:2658-2672`。 |
| 谁受影响 | 等待人答复的轮次、renderer lease、run_reattach、历史替换恢复。证据 `B/src/gateway/user-dialog/FileGatewayUserDialogStore.ts:149-171,227-245`、`B/src/gateway/run/RunRegistry.ts:285-307`。 |
| 能否撤回 | 隔离演练可撤回；真正数据切换需先验证 TS 可读 Rust 落盘，并在回滚时先停止新写者，不能恢复旧备份盖掉新工作。依据 `.42cog/intent.md:26-27`、`B/src/gateway/run/RunRegistry.ts:184-211`。 |
| 动不动尺子 / 意向 | 不新增“所有 pending 跨进程无损续跑”承诺；若提出此功能属于新行为，需另议。依据 `.42cog/intent.md:23,27`。 |
| 选项与后果 | A 内存序列化热迁：需要新协议转移回调/lease/执行责任。B **排空后持久状态恢复**：可审计，保留既有 crash/recovered 行为，有维护窗口。C 全部易失状态持久化：扩大本次范围及崩溃语义。 |
| 建议与分级 | **选 B，自决·待复核**。Rust 保持进程内 generation 与新 connectionId；模块的 `{moduleInstanceId, connectionGeneration}` 与 WS `{connectionId, generation}` 是两个身份域，不能混用。后者见 `B/src/gateway/server/GatewayWsConnection.ts:35-38`，前者见 `B/src/agent/modules/transport/agentLoopSidecarClient.ts:1040-1055`。 |
| 原则 → 断言 | 只恢复有持久协议依据的事实 → run orphan 不能伪装 running/success，旧 binding/旧 module generation 不授权新副作用；live/hosted/recovered dialog 分支分别测。依据 `B/src/gateway/run/RunRegistry.ts:285-307`、`B/src/gateway/client/InProcessGateway.ts:2661-2672`。 |

## D4 · #19 是照抄竞态，还是修正宿主收尾

| 卡项 | 内容 |
|---|---|
| 卡在哪 | current fence 仍有效时，timeout 后 catch 可以多写 gateway_submit_failed；timeout 测试断言只有 turn_timeout。已有 accepted-diffs 又曾把“原版缺陷”更正为“Rust 产品问题”，不能当作已获豁免。证据 `B/src/gateway/client/InProcessGateway.ts:1708-1760`、`B/tests/gateway/operation-deadline.spec.ts:154-160`、`specs/parity/accepted-diffs.md:9-11`。 |
| 谁受影响 | deadline 两场景的帧与 durable.status、UI 错误展示、C/A 分数与 unknown 责任。Module final 权威见 `B/src/agent/modules/transport/agentLoopSidecarClient.ts:662-684,1017-1033`。 |
| 能否撤回 | 实现可撤回；已写的用户状态/已执行副作用不能靠回滚二进制撤销。只在隔离夹具验证后再定行为。 |
| 动不动尺子 / 意向 | 修已证 bug 符合 intent；差异接受与“唯一终态” oracle 可能改变冻结评分语义，**必须等人**，不能从“默认修 bug”推导可自行放宽尺子。依据 `.42cog/intent.md:23`、`CLAUDE.md:123-128`。 |
| 选项与后果 | A 复刻竞态：容易继续出现时序差异，无法保证确定通过。B **精确修 timeout 已取得收尾权后重复失败写入**：保持 unknown ledger，单一可见 timeout；补 bug②证据与差异登记。C 把全部 DEADLINE_EXCEEDED/unknown 统一吞掉或成功化：丢失不确定副作用责任，不接受。 |
| 建议与分级 | **建议 B，等人（A19-HOST 候选）**。先在 A5 用可控 writer/回调顺序重现；只在证据覆盖的 timeout-first 情形修。unknown-first 尚无完整证据，默认保留既有失败处理。#19 继续使用原 issue，不再建一条同义 issue，不因文档写完而关闭。 |
| 原则 → 断言 | 执行未知可以和用户已超时并存；同一 run 的可见收尾只能裁决一次 → timeout-first 时只出现一套 timeout status/error，ledger 仍 unknown；late ACK 不变 known，generic error 与用户取消不被吞；慢/失败持久化都测。依据 `B/tests/gateway/operation-deadline.spec.ts:117-197`、`B/src/agent/modules/transport/sessionOperationLedger.ts:132-164`。 |

## D5 · 原版 UI 自动恢复失败如何验收

| 卡项 | 内容 |
|---|---|
| 卡在哪 | 客户端 close 时先清 hello，bridge 取不到旧 binding；真实 UI 未发送 reconnect/snapshot。Rust gateway 无法可靠替客户端猜 previousBinding。证据 `B/src/gateway/client/GatewayWsClient.ts:93-100`、`B/src/gateway/client/RemoteGateway.ts:111-112`、`B/ui/server/pilotdeck-bridge.js:255-258,2127-2130`，实测见 `docs/experiments/exp003-transparent-proxy/readme.md:55,65-70`。 |
| 谁受影响 | Web 用户断线中的权限/问题交互，A 的人工验收声明；桌面仍不纳入此次变更，依据 `.42cog/intent.md:24`。 |
| 能否撤回 | 文档上的阶段验收分层可撤回；改 UI 越过本轮只写文档范围以及阶段 A 不改 UI 的边界。 |
| 动不动尺子 / 意向 | 扩大 A 改 UI 或缩减“真实 UI 自动恢复”完成条件都需人定；协议探针通过不是自动豁免。依据 `CLAUDE.md:123`、`.42cog/intent.md:24`。 |
| 选项与后果 | A 等阶段 B 修客户端后才关闭完整 UI 闸门：严格但可延后最终声明。B 人批准 A 有一项明确已知限制，允许协议域先验收：不再声称完整 UI 恢复。C 提前改基线 UI：触红线，本任务不能执行。 |
| 建议与分级 | **建议 A，等人**。让 A5–A8 的代码与协议证据继续推进，最终“完整 UI 恢复”保持 BLOCKED；协议/会话域完成与最终用户验收分开记录。不在 gateway 删除 previousBinding 校验来绕过。 |
| 原则 → 断言 | 客户端没发的请求不是服务器恢复成功 → 真 bridge 录帧必须出现 reconnect_interaction、active_turn_snapshot、成功答复与轮次结束；保存 binding 的专用 driver 仅计协议证据。依据 `docs/experiments/exp006-gateway-parity-ruler/readme.md:78`。 |

## D6 · 41 待定与外部 provider 的完成口径

| 卡项 | 内容 |
|---|---|
| 卡在哪 | 默认未配置时 CAPABILITY_UNAVAILABLE 很容易对拍通过，但 skills/memory/snapshot/manager/upload 有 native/external/注入分支；八组代理场景不能证明这些能力。证据 `B/src/cli/createLocalGateway.ts:851-870,1238-1313`、`B/src/cli/GatewayDialogBundle.ts:66-72`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:74-78`。 |
| 谁受影响 | 自定义部署与集成、cron/渠道用户、TS gateway 退役声明。原 CLI 微信回调见 `B/src/cli/pilotdeck.ts:233-247`。 |
| 能否撤回 | 覆盖矩阵与 provider 适配可以逐 PR 修订；隐去能力会改变用户可用功能，不能默认为兼容。 |
| 动不动尺子 / 意向 | 保持固定基线已暴露协议；新增覆盖须走 A5。若只想交付某个配置子集，必须另列范围决策，不能自行缩分母。依据 `.42cog/intent.md:23,27`、`docs/research/005_phase-a-gateway/method-inventory.md:131-135`。 |
| 选项与后果 | A 仅默认配置：快，但不等于完整 A。B **逐 provider/配置分支记录覆盖，成功与不可用都验证**：工作量大，可证实退役范围。C 一律代理 TS：过渡可用，永远不算退役。 |
| 建议与分级 | **选 B，自决·待复核**。A5 补齐 native/disabled/external 清单，A6/A8 逐个实现；未知 provider 作为显式缺口，相关 issue 不完成。如需缩成子集，另提等人卡，不能沿用此建议代批缩减。 |
| 原则 → 断言 | “未配置”不等于“已实现” → 101 项 method coverage manifest 每项绑定输入配置、owner、成功或基线确无能力的依据、负例、帧/落盘证据；遗漏/重复/全走 unavailable/隐形 TS fallback 的对照必须失败。依据 `B/src/gateway/client/InProcessGateway.ts:2088-2117`、`B/src/gateway/server/GatewayWsConnection.ts:261-273`。 |

## 供后续协调者回填的拍板入口

人只需决定 D4 的精确 bug 修正/评分登记和 D5 的最终 UI 闸门；D1/D2/D3/D6 已作为本计划的技术建议，但发车仍须人的指令（`CLAUDE.md:48`）。尺子变更要另开有理由的 issue、两家评审；本计划 A5 就承担这一独立可验收成果，**不意味着此刻允许修改 specs**（`specs/gateway-parity/FROZEN.md:5`）。本轮仅产出调研/计划，不发送 issue 评论、不提交新 issue、不改 state 或 specs。
