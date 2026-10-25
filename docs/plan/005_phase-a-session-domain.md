---
type: plan
number: "005"
date: 2026-10-14
title: 阶段 A 深水区：Rust 宿主、会话权威与 TS gateway 退役
tags: [phase-a, gateway, session-domain, sidecar, deadline]
status: draft
related: [plan/002_phase-a-gateway, plan/001_phase-c-agent-loop-sidecar, research/008_phase-a-session-domain/must-know, research/008_phase-a-session-domain/gap, research/008_phase-a-session-domain/decision, experiments/exp006-gateway-parity-ruler]
---

# 阶段 A 深水区计划（A5 起）

> **现行决策（2026-10-16 接续校正）**：D4→B、D5→A 均已有人批准，依据见[决策记录](../../state/decisions-20261014.md)。下文起草时的“D4/D5 待批／未决”不再是等待条件。D4 仍须在 A5.4 登记 A19-HOST 逐格期望、评审冻结，再由 A7 实现与验证；D5 以 A8「gateway退役验证完成（不含UI自动恢复）」满足 B3 的 A 侧前置，完整 UI 恢复由 B 后续真实验收闭合。批准不等于实施或验收已经完成，A8 全套技术证据与 B1/B2 前置均保留。

## Context

承接计划 002 的“A5 起·深水区”：现在要迁走的是拥有状态的 gateway 宿主，包含会话接纳、agent 宿主能力、binding、交互、历史/运行登记、项目配置与其它入口。架构依据集中在 [调研 008](../research/008_phase-a-session-domain/must-know.md)，91 个未本地化 method 的工作包映射和 #19 因果链在 [gap](../research/008_phase-a-session-domain/gap.md)，决策分级在 [decision](../research/008_phase-a-session-domain/decision.md)。计划 002 对后续编号“003”的占位由本 **005** 承接；003 已用于阶段 B，本轮不重排旧编号，仅在其相关段末追加 D5 待批解环建议，未批准前维持原闸门。

**方案**：按状态所有者分小 PR 构建，默认代理链路保持完整；使用独立 home/project 测试 Rust 原生链路。Rust gateway 通过 Module Protocol 直接驱动阶段 C Rust sidecar，同时承担全部宿主回调。会话域和相关全局写者都准备齐后，在排空边界一次切换权威。没有同一 `PILOT_HOME` 下 TS/Rust 会话状态双写期，也没有逐个打开 (b)(c) 的生产白名单阶段。

本文件是后续执行蓝图，**本轮只写文档，未执行任何下列实现、运行、冻结或发车步骤**。研究中的 `B/` 指只读 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/` @ `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。不运行/修改这份基线、不起服务是本轮边界；以下验收命令供以后明确发车的执行者使用。

## 现状（已确认）

| 项 | 现状 | 证据 |
|---|---|---|
| 源码起点 | 本仓 `07364f8`；gateway 有解析与 dispatch 骨架，本地白名单为空，A4 的 10 个只读不能写成已完成 | `src/pilotdeck-gateway/src/dispatch.rs:1-21,43-59` |
| method 分母 | 101 = (a)10 + (b)46 + (c)4 + 待定41；A4 分母10保留，深水区50按整域验收，41按 provider/全局域验收 | `docs/research/005_phase-a-gateway/method-inventory.md:13,127-135` |
| 尺子 | A2 与 A3 已冻结；exp006 八组代理回归不能代表 101 method 或原生宿主 | `specs/gateway-parity/FROZEN.md:3-6`、`specs/gateway-parity/schema-FROZEN.md:1`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:74-78,97-98` |
| 连接/交互权威 | hello binding、pending、current/last binding 和等待回调由 TS 持有；权限 SDK 控制连接有特例 | `B/src/gateway/server/GatewayWsConnection.ts:29-63,492-505`、`B/src/interaction/InteractionReconnect.ts:67-70` |
| sidecar 关系 | Rust sidecar 已有 Module 调用输出，但 model/tool/context 等生产宿主能力不因此自动存在 | `src/pilotdeck-transport/src/stdio.rs:469-485`、`B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:394-437` |
| #19 | 最新交接保留 deadline 两场景的宿主竞态；新计划继承现有 issue，不自动关闭、不当已批准差异 | [#19 最新评论](https://github.com/zhengdaqi/pilotdeck-rs/issues/19#issuecomment-6160592863)、`specs/parity/accepted-diffs.md:9-11`、`B/src/gateway/client/InProcessGateway.ts:1727-1759`；解释见调研 gap §四 |
| UI | 真 bridge 自动恢复失败；协议保存 binding 的回放通过不含答复/轮次完成 | `docs/experiments/exp003-transparent-proxy/readme.md:55,65-70`、`docs/experiments/exp006-gateway-parity-ruler/readme.md:78` |
| 环境 | 所有未来验收用高位端口、隔离 home/token/project/database/tmp，只有本次编排自己的进程可清理；全量共用串行锁 | `CLAUDE.md:69-88`、`scripts/gateway-parity/README.md:17,33-39` |

## 目标

1. **宿主可替换**：Rust 有 Module client、生产 host providers、durable ledger 与 session lifecycle；四格矩阵能区分 A 宿主错误与 C sidecar 错误。使用 mock 外部服务不等于允许 mock 掉被迁移的 Rust 宿主。
2. **会话域完整**：调研 gap §二的 50 个 method 与 `hello`、notification、close 全由一侧状态权威负责；交互可成功答复并结束轮次；历史与在飞快照一致；#19 确定性回归关闭。
3. **TS gateway/内部宿主退役**：41 待定逐组完成，101 method 的成功/错误/能力分支有覆盖；18789 所代表的 WS/HTTP/后台职责全部由 Rust 承担。实际验收仍只绑定高位端口，不动用户正在运行的实例。TS ui/server、TS 测试驱动和用于对照的 TS sidecar不属于“gateway 未退役”的偷渡判据；TS host-helper 则属于未退役。基线配置原本选择的外部 provider 按原协议连接，不要求把外部服务也重写；但不得把尚未移植的 TS 内部模块临时包装成 external provider 来宣称退役。
4. **声明分层**：A6 宿主 contract 完成、A7 会话域原生隔离验证完成、A8 gateway 退役验证完成、人工 Web/自动恢复验收分别记状态。D5 未决时不得宣称完整用户验收；Web 通过不能宣称桌面兼容。

完成口径维持三个账本：A4 本地只读 `10/10`；A7 **一个完整会话域 PASS/FAIL**（附50项覆盖，禁止49/50就切换）；A8 `101/101 method coverage + 全 HTTP/后台入口 + 无 TS host`。所有“通过”都要求实际本地实现与语义一致，纯透传不计进展；获批 bug 差异按精确条件登记，不能抹去未知错误或更换分母。

## 执行步骤（分 issue，issue 内分小 PR）

**依赖顺序**：A5 定标/装置 → A6 宿主和存储 → A7 会话域 → A8 旁路入口与整体切换。A4 与 A5 研究可独立推进；A7 原生整域验收及 A8 切换要求 A4 `10/10` 已有证据。#19 沿用现有 issue，A5 提供复现，A7 的收尾 PR 实现；D4 未批不能改变对拍评分语义，D5 未批不能关闭完整 UI 门。

**与 B3 的解环草案（D5 等人，未生效）**：若人采纳 D5 建议 A，B3 所需的“阶段 A 对拍达标”具体指 **A8「gateway 退役验证完成（不含 UI 自动恢复）」**：A4/A6/A7 前置齐备，101 method、全部 gateway HTTP/后台入口、无 TS host、隔离切换/重启/回滚、E0/E1 retirement 和已批准口径下的完整 gateway 回归通过，阶段 P2 依赖清完。候选顺序为 **A8 gateway 里程碑 → B3（另须满足 B1/B2 自身前置）→ B 后续客户端恢复修复 → 真实 UI 自动恢复验收 → 完整用户验收/本计划 done**。A8 gateway 里程碑不依赖最后的 UI 门；仅 A7 会话协议域通过也不满足 B3。B 的修复工作包须显式登记，不能声称现有 B3 已包含它。这样保留完整 UI 承诺而不让 A 与 B 互等；D5 未批时只能记录 gateway 证据与 UI BLOCKED，不能据此自动放行 B3。[计划 003](003_phase-b-ui-server.md) 同步保留这一待批说明。

**粒度铁律**：每条 issue 对应一个独立可验收结果，不按 method 建几十条 issue；工程量大仍留在同一 issue 中拆小 PR。下列估算是静态调研的规划区间，不是源码事实或工期承诺。预计超过3天的 issue 不整条无人值守发车；每次只派一个 **0.5–3天**、有自己证据与回归的小 PR。如果表内某 PR 开工细化后超过3天，就继续按 provider/操作/配置分支在原 issue 内拆分，先补具体输入、输出、负例、依赖再派工，不以“基础设施”打包无限工作。依据为共享 `dev-launch/SKILL.md` §3 粒度铁律；本轮不执行其发车、评论、提交流程。

每个代码 PR 通用完成条件：冻结文件检查、受影响 contract/负例、当前已有场景回归、CI check、原始输出与进程身份归档；按 CLAUDE.md 走非作者 simplify/pr-ready 和双谱系评审，无 P0/P1。依赖上的 P2 先清，阶段结束清一次 P2-跟进。未开启的原生组件可以合并，但默认代理回归不能退步、不能声称已接管。

**冻结与小PR不能互相打架**：A5若某个PR修改已冻结脚本或新增白名单内脚本，该PR须附变更理由、受影响规则、双评审及必要的人批，获准后在同PR登记新hash并采集对应证据；未获准就保持draft。不能让A5.1–A5.3先带冻结校验失败合并，再等A5.4补登记。A5.4是完整定标与最终口径确认；D4未决的语义变更单独留draft，不混入已批准的兼容性扩展。

### Issue A5 · 深水区覆盖与判据定标（新 issue；独立尺子变更，不改产品行为）

**成果**：可信的新宿主/会话域装置及 coverage manifest，包含 #19 可控交错。预计5–9天，拆4个小 PR，每个1–3天；决策等待不算工时。

| PR | 工作 / 目标位置 | 自己怎么算做完 |
|---|---|---|
| A5.1 依赖账本 | 基于研究 gap 的50+41列表，扩展 `scripts/gateway-parity/` 的 coverage manifest（拟新增）；逐项列 handler→provider→存储/内存→通知→成功/错误/未配置分支。补 memory/snapshot/browser、external binding 的实际提供者、配置和释放证据 | method 集合与冻结 schema/101清单一一相等、无重复；每项有 owner 和场景ID；未知项明确 BLOCKED；补全后才估算 A6 provider 子 PR |
| A5.2 并发/重启夹具 | 复用官方 client、独立 driver 录帧、进程管理与高位端口隔离；新增双连接、多会话、旧close/新reconnect交错、store 故障、可控 clock/barrier、cold-restart；#19 实测复现模型/工具等待 × timeout/unknown 顺序 × **有/无 runRegistry × 快/慢/失败 writer**，分别控制 status 写入与 registry append、queue.close、外层 complete 边界 | 每个夹具有独立 oracle；不能靠 sleep 调到“碰巧绿”；真 bridge 重连另列；实时帧、durable status、registry events/result 分开断言，六格不得缺项；有 registry 组须覆盖生产默认 FileRunRegistry，结果待实测，不预判 timeout 帧可见或 result 不变，详见下表 |
| A5.3 原生拓扑与四格入口 | 拟新增 `scripts/gateway-parity/session_domain.py`，接口见下；复用现有 compare/driver 而不复制一套尺子。新增 Rust-host→TS/Rust-sidecar、禁 TS host-helper 审计；连接数、进程树、argv/hash、输出/落盘/HTTP 都记账 | native 拓扑被声明时，启动 TS gateway、调用隐形 helper、漏一组场景、伪造 unavailable、丢/重/乱序事件、错误 binding、空 observer、两侧同坏都须 FAIL |
| A5.4 定标/评审/冻结 | TS-only 先校准新增组 k=5；#19 已知非稳定交错单列，受控预期与批准差异明确，不能删场景。按独立 issue 理由更新装置白名单/哈希、acceptance/normalization 与差异登记；D4 的评分语义、人审门另记 | 原八组仍通过；新增各组有完整5轮与全对比较、33项旧 controls 及新增 controls；两家评审无P0/P1；语义变化依 D4 人批后冻结，未批只交草稿不伪称通过 |

**#19 在此的工作**：实测复现调研 gap §四的静态机制，不预先写定基线帧或落盘结果。运行期 handle 到外层 finally complete 才移除，timeout writer 返回/queue.close 不使其失效；另有接纳前 releasePreflight，不混入运行期交错。无 registry 时直接入队；有 registry 时 append 成功后才入队，timeout 发布后同步关 queue 可能丢失自身帧；FileRunRegistry 的 failed 不拒绝后续 append，后到 error 还可覆盖 result。生产 createLocalGateway 默认总有 registry，不能以无 registry 夹具代替生产证据。

A5.2 后续发车时只读基线、不改资源源码，用独立 fixture/注入的 session/storage 及 barrier 控制交错，真实 sidecar 场景交叉确认。**必须实测下列六格**；这里的“快/慢/失败 writer”须拆到两个写入点：durable status writer 与 registry append，二者可独立快/慢/失败，不能只延迟 status 写入就声称已覆盖 registry。无 registry 组只有 status writer；有 registry 组分别让两个写入点快写、受控阻塞、独立失败，并覆盖二者交叉快慢及故障交错。有 registry 组必须使用生产默认 FileRunRegistry 的实际存储语义，记录注入方式，不能只凭 mock append 宣称复现生产。

| 待实测拓扑 / writer | 受控次序与复现问题 | 帧观察 / 独立断言 | durable status 观察 / 独立断言 | registry events/result 观察 / 独立断言 |
|---|---|---|---|---|
| 无 registry / 快 | timeout-first，status 快写，控制关 queue 后 unknown 进入仍 current 的 catch | 录原始帧、缺失与顺序，不预填 timeout 可见 | 读实际存储，核对 timeout / gateway_submit_failed 的次数和顺序 | 明确未配置、不适用，不冒充已验证 registry |
| 无 registry / 慢 | 阻塞 timeout status 写入，控制 catch 写入、入队、释放 writer 与关 queue 次序 | 核对各帧实际入队/消费次序 | 独立核对写入尝试、提交与顺序，不从帧反推 | 明确未配置、不适用 |
| 无 registry / 失败 | 分别使 timeout / catch status 写失败，保留各自错误证据 | 核实错误帧是否可见及实际 code | 核实实际持久内容，不把写入尝试算成功 | 明确未配置、不适用 |
| 有 registry / 快 | status 与 append 快写，显式观测 append 的 .then 与同步 queue.close 次序；控制迟到 catch | 核实 timeout 自身帧及后续失败帧是否缺失，不预判可见 | 独立读取 status 的实际内容与顺序 | 读取 events/seq 与 record.state/result，核实 timeout 与迟到 error 的追加及 result 是否覆盖 |
| 有 registry / 慢 | 独立阻塞 status 或 append，覆盖快 status/慢 append、慢 status/快 append 及二者均慢；控制关闭前后完成 append | 比较实际帧与 append 完成、queue.close 的顺序 | 独立核实 status 内容，不能拿 registry 代替 | 核实实际 events 顺序与最终 result，不以帧不可见判定 append 未发生 |
| 有 registry / 失败 | status 与 registry append 分别注入失败，再覆盖失败与另一写入点快/慢的交错 | 记录 timeout / result_unknown / gateway_submit_failed 等实际错误与缺帧 | 核实已提交 status 与失败尝试，保留原始错误 | 核实 events/result 的实际持久结果、state/revision/seq；故障后从磁盘重读，不能用内存变更或 rejected promise 推断落盘成功/失败 |

六格各自保留原始证据与独立断言，**具体基线输出在实测复现后回填**；先登记受控次序、观察字段和完整性检查，不把上述问题当成预定 PASS 结果。另分别覆盖 unknown-first / known-final-first，以及 unknown 迟至外层 complete 后的旧 handle fence 边界；记录无法复现、被其它终态挡住或 bug②证据不足的情形。用“丢 timeout 帧”“漏读 registry events”“忽略 result 覆盖”等独立失败对照证明各观察面能检出差异。

**D4 选 B 的候选 Rust oracle（待批，非基线结果）**须另列：已定标的 timeout-first 成功持久化分支中，帧仅一套 timeout status/error，durable status 无额外 gateway_submit_failed；有 registry 时 events 无迟到失败、result 保持 timeout，无 registry 时明确不适用；ledger 仍 unknown。失败分支不伪称持久化成功，具体错误发布与落盘口径在复现后交人批准；D4 未批不能以候选规则改评分。源码依据 `B/src/cli/createLocalGateway.ts:996`、`B/src/gateway/client/InProcessGateway.ts:974-986,1216-1266,1384-1439,1727-1759,1848`、`B/src/gateway/run/RunRegistry.ts:242-260`、`B/src/gateway/util/AsyncQueue.ts:17-18`。

**怎么算做完**：上述四 PR 各自完成；101清单及 provider 分支无无主项；原生拓扑与错误对照可判；D4 涉及的规则未获批时 A5 标“兼容部分完成，修正口径待批”。现有 A2 八组全绿不是 A5 的替代物。

**验收命令**：现有检查块 E0；A5 实现并批准后跑 E1 的 `--suite calibrate`（包含 #19 的 TS-only 受控复现和装置负例）。完整四格 `--suite deadline` 留到 A7 新宿主具备后运行。结果中所有必需组必须非空，状态不含隐式 skip；未能绑定 loopback 时交协调者执行，不修改判据。

### Issue A6 · Rust 宿主能力与持久状态（新 issue；默认 WS 仍透传）

**成果**：Rust 可以真实承载 sidecar 回调与 session 持久化，给 A7 提供完整 host；不能只用假 model/tool 让 loop 空转。粗估20–40天起，外部 provider 待 A5 清单重估。按下列包拆 PR，每包内每个 provider/操作族1–3天；**不是一条20天的无人值守任务**。

拟实现放 `src/pilotdeck-gateway/src/{host,storage,runtime}/` 等逻辑模块；是否分 crate 以出现的依赖为依据，本计划不提前扩 workspace。复用现有 `pilotdeck-protocol` 与阶段 C 的适用协议类型；同名 `pilotdeck-transport` 现为 sidecar 侧运输，实现宿主 client 前须确认调用方向。

| 小 PR 包（必要时同包多 PR） | 复用/参考与实现内容 | 局部验收 |
|---|---|---|
| A6.1 Module host transport | `B/src/agent/modules/transport/agentLoopSidecarClient.ts`、`stdioAgentLoopSidecarConnection.ts`；spawn/handshake/execute、stream identity/seq/ACK、module_call、cancel、退出与重连/replay。先协议 client，再 replay/cancel 两个 PR | TS/Rust sidecar 交叉测试；错误 identity、重复冲突、旧 generation、accepted 后断线 fail-closed；双向 I/O 不因等待 callback 死锁；子进程有界清理 |
| A6.2 Durable session / ledger | `sessionOperationLedger.ts`、`B/src/gateway/run/RunRegistry.ts`；transcript/metadata、started/accepted/unknown/terminal、文件锁/原子替换、revision/seq、owner orphan。ledger、run、历史事务分 PR | append 失败不可先发布成功；重复run不再 execute；重启恢复与坏尾记录符合原 provider；known/unknown 不互相伪造，旧数据可读 |
| A6.3 Config / project generations | `B/src/cli/ProjectRuntimeRegistry.ts`、`GatewayRuntimeRefreshBundle.ts`、`GatewayCommandCatalogBundle.ts`；snapshot/reload、provider/plugin lease、SDK overrides、permission规则 | config→invalidate/dirty→broadcast 顺序；活跃 lease 退休后释放；同轮与下轮使用哪代配置有明确定标 |
| A6.4 Model / budget providers | `B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:67-151,394-416`；真实 provider 适配、prepare/stream/next/close、routing/metadata、usage/cost/task budget。按 provider与操作族分别 PR | 用本地 mock HTTP 服务替外部模型；实际 Rust request/stream 处理被测；token/预算/失败语义与 host contract 一致 |
| A6.5 Tools / MCP / background | `B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:153-189,430-435` 与 `B/src/cli/ProjectRuntimeRegistry.ts:1293-1310,1707-1751`；工具 registry、batch/cancel、副作用权限、MCP 连接、subagent/background/file history。逐工具族、MCP、后台各 PR | 工具实际副作用与取消责任可观察；拒绝/超时不执行新 effect；已发送未确认保持 unknown；后台资源能停止并排空 |
| A6.6 Context / memory / snapshots | `B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:308-376`、`B/src/cli/ProjectMemoryBundle.ts`、`B/src/cli/createLocalGateway.ts:615-620,1271-1313`；prepare/apply/capture/compact 与持久化、memory provider、workspace pre/post recorder。各 provider 分 PR | capture/compact 与 durable transcript 同步；memory/snapshot 成功与未配置都测；不会凭一个 mock port 声称生产 provider 已迁 |
| A6.7 Hook / event / steer services | `B/src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts:190-207,269-307,378-391`、`B/src/gateway/client/InProcessGateway.ts:694-729,1896-1997`；生命周期/事件、steer drain、async callback identity | callback duplicate/expired/late、hook drain、pending 接纳与关闭均可判；异步 handler 不越过旧 run fence |
| A6.8 Dialog / upload / skills provider | `B/src/gateway/user-dialog/FileGatewayUserDialogStore.ts`、`B/src/gateway/user-dialog/GatewayUserDialogBus.ts`、`B/src/gateway/dialog/UploadStore.ts`、`B/src/cli/createLocalGateway.ts:851-870`；按 dialog store、upload、skill binding 分 PR；先用进程内 contract 驱动验证等待方/回调，A7再接真实WS连接owner | live/hosted/recovered 的 lease/answer 传递，upload pin/release/expiry，native/disabled/external skill 选择符合基线 |
| A6.9 Host integration | 按实际 provider capabilities 合成 Module manifest；接已有 agent runtime，而非重写 loop | 四格矩阵相同 fixture 下语义一致；D4 已批差异精确命中；未支持 provider 明确阻断 A6 完成；不改默认代理路由 |

**怎么算做完**

- [ ] A5 provider 清单的每个必需分支有真实 Rust 实现、正/负 contract 测试和资源释放证据；没有“TODO但 advertised=true”。基线未配置分支按原行为保留。
- [ ] transcript、operation ledger、run registry 的写入/发布顺序、崩溃恢复、重复提交、跨会话隔离通过；数据格式变化必须单独说明并验证兼容，不私加数据库迁移。
- [ ] Rust-host→TS-sidecar 与 Rust-host→Rust-sidecar 都有实际运行账本；既有 C 对拍单独回归，不能用新 host 消掉的差异倒算 C 原分数。
- [ ] 默认代理链路回归全绿，未放开孤立 (b)(c) handler；所有 PR 有 E0 与受影响 E1 host 证据，结束跑完整 host 矩阵。

**验收命令**：E0；E1 `--suite host`。每个 provider 的小 PR 可以跑清单中受影响的子组（A5 定义过滤参数和必测数量），A6 完成必须跑完整组，过滤结果不能充作完成报告。

### Issue A7 · 完整会话域与 #19 Rust 宿主收尾（新会话域 issue，关联并复用 #19）

**成果**：隔离原生实例的50项会话域完整可用，并收掉 `hook_async_result`、`read_subagent_messages` 两个依赖；默认环境仍保留 TS 权威，直到 A8 全入口切换。粗估15–25天，分下面8包，每包1–3天；SOP/always-on/provider分支细化超3天时继续拆同 issue 小 PR。

| PR 包 | 实现顺序与目标位置（拟新增） | 自己怎么算做完 |
|---|---|---|
| A7.1 接纳与连接 owner | `runtime/session` 与连接 context；对齐 SessionRouter/binding/auth/clientName，运行时 epoch/handle；hello 与 describe 共源 | 同会话重入拒绝、跨会话不互锁、带runId的旧abort不改新run；无runId旧socket close按A5定标，不预先修正疑似竞态；两种generation不混用；连接状态错误格式对齐 |
| A7.2 submit 与事件账本 | `runtime/turn`；A6 host 接入，WS 请求 seq、run registry seq、Module seq 分开；durable append→可见流、replay 500/256KiB/30s、末帧与 notification 顺序 | 文本/工具轮次完整；history epoch 变化能重试/报错；注入丢/重复/提前final必须失败；不把同一终态的普通事件与最终 WS 包装误判为两个业务裁决 |
| A7.3 permission / elicitation | `runtime/interaction`；pending producer/consumer、reconnect、SDK 独立控制连接、close/timeout cleanup | 旧binding/重复/过期答复拒绝；断线pending保留与无pending abort均符合TS；重新绑定后答复成功并结束真实轮次，不止 snapshot 有数据 |
| A7.4 user_dialog | A6 store接通校验、claim/renew/release/expiry、hosted→live→recovered、broadcast | 两renderer争租约、错误值不消费、崩溃后恢复、owner租约与renderer租约分离；notify失败不改变bus/store权威 |
| A7.5 历史与会话写事务 | `storage/history`、`runtime/replacement`；S1/S3/S5，A6 catalog、fork/replace/finalize、session model/config、read_subagent_messages恢复 | close/delete/export/restore忙闲边界、替换claim/commit/rollback、半写/重启、读epoch校验；原A4读方法看到同一新状态 |
| A7.6 运行中控制/统计 | S2/S6与hook_async_result；steer/cancel/abort、usage、background、MCP status、seed/rewind | late/duplicate hook不注入新轮次；abort drain有界；后台任务真停、usage归属正确，不拿空列表过关 |
| A7.7 #19 收尾修复 | `runtime/turn` 的单一收尾裁决，复用 A6 operation ledger；按D4批准的精确范围修，关联原 #19，不另建同义 issue | timeout-first/unknown-first/known-final-first、有/无 registry × 快/慢/失败 writer、Requested、late ACK、sidecar crash 全部有断言；帧、durable status、registry events/result 分开验证，unknown仍入账且未知effect不再执行；两条deadline各5对通过已批准口径，完整C/A回归无新差异 |
| A7.8 自动化与整域装配 | S7 SOP/always-on；能力声明、所有50项与U9/U10一起装配；测试时HTTP同域入口也直达这个owner或明确尚未开放，不能回TS修改同域 | 50项覆盖无漏项；全会话域报告PASS；无TS内部宿主；真实Web普通轮次/权限/历史可验，D5自动恢复仍单列；默认代理仍不进行半域切换 |

**#19 修正的原则与范围（D4 等人，以下为候选）**：分别维护“执行事实 unknown/known”和“可见收尾 open/settling/settled”。timeout 首先取得当前 run 的收尾权并 abort，随后等待该等待的状态写入；迟到 reconciliation 错误仍保留 ledger unknown，但不能再次追加 durable gateway_submit_failed 或发布对应失败帧。**只在 D4 已定标且获批的 timeout-first 情形应用，覆盖有/无 registry × 快/慢/失败 writer，分别核验帧、durable status、registry events/result，不以阻塞或帧可见为条件**；unknown-first 保持原失败投影，已知 final 与 timeout 竞合采用 A5 预注册次序。禁止改成成功、删除未知状态、重发工具、人为延时、改归一化屏蔽错误。#19 在代码、精确差异登记、回归证据齐备并按流程回复之后才能关闭。

**怎么算做完**

- [ ] C/S1–S8 全覆盖，U9/U10 同步；不是通过几个 submit fixture 就称“会话迁完”。原生数据目录中没有 TS 会话写者。
- [ ] reconnect→pending replay→有效answer→终态闭环通过；基线 `delivered=false` 现象已定因，若属修 bug 有独立证据/批准差异，不直接改成 true。
- [ ] #19 的 Rust 收尾与判据均已闭环；一般错误、Requested、budget、工具副作用未知的行为无回退。
- [ ] A4 `10/10` 与 A6 provider 依赖均有证据；E0、E1 session/deadline/host 全过；50项作为一个 PASS/FAIL交付。
- [ ] 人工 Web 状态明确记录；D5 未解决则只交付“会话协议域完成”，不关闭完整UI恢复门，更不宣称整个阶段A完成。

**验收命令**：E0；E1 `--suite session`、`--suite deadline`、`--suite host`。使用 A5 定义的四格矩阵/同路径归档；#19 的模型/工具场景 k=5 是必要项，受控交错测试是防偶然5次通过的另一项必要证据。

### Issue A8 · 全局/旁路入口接管与 TS gateway 退役（新 issue）

**成果**：同一 Rust authority 覆盖剩余41项和 gateway HTTP/后台职责，冷切换与回滚演练可审计。初估15–30天起，取决于 A5 外部provider与HTTP入口清单；分下列小 PR，单 PR 1–3天。A6已完成的provider不重写，A7已实现U9/U10只做总账核验。

| PR 包 | 工作（复用 A6/A7 owner） | 局部完成条件 |
|---|---|---|
| A8.1 全局配置与扩展 | U5/U6/U7/U11；settings/MCP/commands/skills/output styles/reload；按配置、MCP、skills三个或更多小 PR | 所有读写/重连走同一generation；dirty、lease释放、notification顺序正确；成功/未配置/外部provider都覆盖 |
| A8.2 上传与管理 | U1/U2/U3/U4；按upload、memory、snapshot、manager分PR | 附件轮次持lease、过期/取消/泄漏清理；snapshot workspace隔离；manager成功路径有实际provider；未核分支不算完成 |
| A8.3 自动触发与channel | U8/U12；cron scheduler、SOP/always-on入口核验、微信登录wiring、feishu/channel生命周期；每触发源独立PR | 同一接纳器，重复触发/取消/重启不重执行；旧调度器停止；mock外部服务证明渠道状态/回调，避免真实账号动作 |
| A8.4 gateway HTTP面 | `B/src/gateway/server/GatewayServer.ts:96-131`：health/local-token、api/web、module-host describe/call、approvals、webhook、static；按共享authority/路由组分PR | WS与HTTP写同一状态并互相可见；鉴权、错误、真实生产流式路由、静态回退都验；不能以测试专用SSE替代生产流 |
| A8.5 统一能力与切换 | U13 describe/hello；停止接纳→drain所有入口→flush→停旧owner→Rust恢复→新握手；保留代理启动模式供隔离回滚演练 | 101 manifest全覆盖、hello/describe一致；旧进程/后台writer退出前不放开新owner；drain失败明确停切换 |
| A8.6 退役/回滚验收 | 运行E1 retirement，原生bin无需TS gateway/upstream配置；独立进程/连接观测加“故意偷偷回退TS”对照；Rust写盘后排空再TS读回的隔离演练 | 进程树无TS gateway/helper，网络无TS业务fallback；全部HTTP/后台入口覆盖；TS只读/恢复Rust数据的证据明确；回滚不覆盖切换后新数据 |

**怎么算做完**

- [ ] 调研gap U1–U13的41项全部对应实现和批准的配置覆盖；A4 10 + A7 50 + 待定41无漏/重项，全部101不能靠透传或统一unavailable凑齐。
- [ ] 完整HTTP route manifest、内部host callback、cron/channel/background均已接管；没有因WS全绿而遗留TS HTTP/后台owner。
- [ ] native轮只启动Rust gateway与Rust sidecar及明确外部mock，原版ui/server/测试driver可在独立角色运行；身份审计和故意fallback对照均有效。
- [ ] 冷切换/重启/回滚在隔离数据上完成；用户18789、3001、真实home和进程从未接触。
- [ ] E0、E1 retirement及完整 gateway 回归通过；阶段P2依赖清完。与上述 A8 条件共同形成“gateway 退役验证完成（不含 UI 自动恢复）”里程碑；只有 D5 的 A8→B3 闸门获人批准后，才可用它满足 B3 的 A 侧前置。完整 UI 门未闭合则报告“gateway退役验证完成，完整Web自动恢复验收阻塞”，不标计划done；人工实效验收与桌面边界明确。

**验收命令**：E0；E1 `--suite retirement`，它必须包含101 coverage、生产HTTP/后台入口、无TS宿主审计、cold-restart/rollback、阳性对照。原 `run_round.py --side rust` 默认仍是Rust前置+TS上游，不得拿它作为退役证据。

## 验收（端到端命令与报告）

### E0 · 现有命令（后续发车时运行，本轮未运行）

在仓根执行；Rust检查只在代码PR需要，文档PR不为此编译全仓。运行前按 `scripts/gateway-parity/README.md:3-17` 准备已有依赖，不下载预编译二进制。schema与冻结检查失败就停止相应迁移，不更新哈希掩盖。

```bash
git diff --check
python3 scripts/gateway-parity/check_frozen.py
python3 scripts/gateway-parity/check_schema.py
python3 -I scripts/parity/check_frozen.py

CARGO_HOME="$PWD/_build/cargo-home" cargo fmt --manifest-path src/Cargo.toml --all --check
CARGO_HOME="$PWD/_build/cargo-home" cargo clippy --manifest-path src/Cargo.toml --workspace --all-targets --locked -- -D warnings
CARGO_HOME="$PWD/_build/cargo-home" cargo test --manifest-path src/Cargo.toml --workspace --locked

# 在线校准/原八组回归；必须允许loopback，且只在获准发车后运行。
# 每次替换为新的output，现有原件不覆盖；装置自己持有全量串行锁。
python3 scripts/gateway-parity/check_ruler.py --output _tmp/a5-gateway-calibration
```

`check_ruler` 的机器项绿但 independent_review 缺失可能返回2；必须报告待评审，不能当exit0。以上在线命令对应当前快照的透明代理拓扑，A2默认要求十个只读请求仍转发；A4若已启用本地method，应使用它已获批的本地化验收配方，并保留原透明代理对照模式。不能用A2全透传判据量原生模式，也不能删除no-forward证据。冻结与schema输入应在采集起止核对，产物和装置hash绑定；无服务权限就交协调者采集，不以旧档案复算替新实测。

### E1 · A5 拟新增命令接口（当前不存在，禁止冒充已能运行）

以下是 A5 必须落地并评审的 CLI 约定；实现完成前这些命令的状态就是 **NOT_IMPLEMENTED**，不得据此报任何PASS。每个suite按冻结的场景/配置manifest运行并输出 `report.json`：expected/executed case集合、topology/host/sidecar身份、k轮账本、帧/落盘/HTTP比较、独立oracle、controls、冻结hash、进程/端口/cleanup审计。缺字段/缺组/0 tests/未知skip一律FAIL；未批准差异或人审缺项是BLOCKED，不能PASS。

```bash
# 校准只跑TS基准和装置controls，不能看Rust结果调阈值。
python3 scripts/gateway-parity/session_domain.py --suite calibrate --output _tmp/a5-session-calibration

# host: 四格矩阵；TS-host/TS-sidecar、TS-host/Rust-sidecar、
# Rust-host/TS-sidecar、Rust-host/Rust-sidecar，分别报告而非揉成一个分数。
python3 scripts/gateway-parity/session_domain.py --suite host --output _tmp/a6-host-matrix
python3 scripts/gateway-parity/session_domain.py --suite session --output _tmp/a7-session-domain
python3 scripts/gateway-parity/session_domain.py --suite deadline --output _tmp/a7-issue19
python3 scripts/gateway-parity/session_domain.py --suite retirement --output _tmp/a8-retirement
```

所有suite复用串行锁和相同实际路径轮换归档，不并发全量跑；校准与对拍用k=5（对不稳定/多态场景走已有批准流程，绝不自行删分母）。跨连接不比较调度噪声，但同连接帧全序、请求seq、run seq、lease/expiry判定与Module identity必须保留。必须有如下独立失败对照：旧binding被误接受、双答复成功、unknown变success/工具被重发、duplicate终态落盘、漏通知、空coverage、假local实透传、隐藏TS宿主、两侧相同坏输出。对照必须命中预注册规则，而不只是“程序退出了”。

最后由人用原版 Web UI（standalone unmanaged，env指向高位Rust，隔离token/database/workspaces）验普通轮次、权限/问题、历史、关闭/重启；真实自动恢复按D5决议单列。桌面未验收。全阶段完成记录须引用当次报告，不能引用exp006替代新域证据。

## 风险与权衡

| 风险 | 如果发生 | 处理/停止线 |
|---|---|---|
| 宿主规模被低估 | A6某provider需要超过3天或有未核外部实现 | 在同issue继续拆小PR并重估；相关能力保持未完成，不新加TS helper作为“最终实现” |
| 会话/配置双权威 | Rust会话写本地，TS仍reload/cron/HTTP写同域 | 阻止切换，所有入口清单重新核；仅隔离实例验证，不能靠双写同步“先凑合” |
| 收尾死锁/迟到写 | drain等待callback，callback又等被持有的session锁；旧run回调改新状态 | I/O前保留identity、锁外等待、回来重验；有界abort；barrier负例强制覆盖 |
| #19被误归类 | 测试断言与真实四格行为不一致 | 保留原始失败；D4只覆盖证实条件；不得因intent默认修就扩大已接受差异 |
| 持久化兼容失真 | Rust能读TS，TS不能读Rust；替换事务/lease恢复写坏数据 | 隔离双向恢复演练失败就不切；记录副作用文件与原始错误；不恢复旧快照覆盖新工作 |
| 假退役 | 没有WS上游帧，但HTTP、provider或sidecar callback仍调TS | 进程/网络身份审计与强制fallback对照；101/HTTP/后台三张清单都清零才放行 |
| 虚假覆盖 | 所有可选能力均没配置，全部unavailable | native/external/disabled按基线配置分支测试；未知分支阻断完成，缩范围须另拍板 |
| UI自动恢复仍失败 | Rust协议闭环正常，原bridge没有恢复请求 | 按D5保持最终UI门阻塞；不替客户端猜binding，不改变校验来“修好” |
| 尺子/装置漂移 | 增加脚本被白名单拒绝或修改冻结输入 | A5独立issue、理由、两家评审、语义人批；新hash登记后采新轮次，不静默绕过 |
| 环境/负载 | loopback受限、全量锁已占、磁盘逼近30GB | 不启服务、不抢活owner；交协调者或等待队列；≤5 worktree、全量同时1个、日志归档，不碰用户进程 |

## 范围外

本轮不写代码、specs、`.42cog/`、CLAUDE.md、state或旧研究；不发车、不运行基线、不起服务。后续实施不升级cc7c4e6、不改原版ui/server/desktop、不做无停机内存热迁、分布式多gateway共享会话、agent loop进程内化、性能优化或对外发布。阶段B、前端和桌面启动改造是各自计划；若D5要求越界修UI，须另有明确授权。**这些范围外不包含gateway自身HTTP面、宿主provider或41待定**，它们是TS gateway退役的必需工作。

## 拍板记录与相关

本轮技术建议：D1整域冷切换、D2进程式Rust→Rust、D3只恢复有协议依据的持久状态、D6完整配置覆盖，均为自决·待复核；D4精确bug修正及评分、D5最终UI门为等人。卡的完整理由/后果/原则/断言只保留在 [008 decision](../research/008_phase-a-session-domain/decision.md)，不另抄正文。默认保留当前协议与评分行为，未决不阻断独立调研和兼容实现。

相关：[计划002](002_phase-a-gateway.md) · [计划001](001_phase-c-agent-loop-sidecar.md) · [008 gap](../research/008_phase-a-session-domain/gap.md) · [exp002](../experiments/exp002-connection-state/readme.md) · [exp003](../experiments/exp003-transparent-proxy/readme.md) · [exp006](../experiments/exp006-gateway-parity-ruler/readme.md) · [issue #19](https://github.com/zhengdaqi/pilotdeck-rs/issues/19)。
