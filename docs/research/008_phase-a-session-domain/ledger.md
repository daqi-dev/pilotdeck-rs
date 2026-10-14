---
type: research
number: "008"
date: 2026-10-14
title: A5.1 依赖账本：静态证据、配置边界与更正
tags: [phase-a, coverage, ledger, providers]
status: 静态账本完成；未闭合依赖显式 BLOCKED
---

# A5.1 依赖账本

正本为 [ledger.json](../../../specs/gateway-coverage/ledger.json)，验证入口为 [check_ledger.py](../../../scripts/gateway-coverage/check_ledger.py)。本页只解释口径与更正，不复制逐方法数据。只读基线 `B/` 为 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/`，已核实 HEAD 为冻结 commit `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；未运行基线。

**口径（自决·待复核）**：账本是**规划索引**。下游 A6/A7/A8 每个 PR 动手前必须复核所涉 method 的 provider/state/guard/error，并回写账本。校验器负责结构完整（集合、行号、领域码扫描、默认构造点）；单项语义细节由下游复核兜底，后续同类细节漏写按 P2 处理。

**「默认」的唯一定义**：`B/src/cli/pilotdeck.ts:107`–`B/src/cli/pilotdeck.ts:116` 的 `createLocalGateway` 调用，只传 projectRoot/pilotHome/env/fallbackProjectRoot/initialAutomation/telemetry/sessionCatalog/sessionDataPlane。userDialogStore、alwaysOnStorageProvider、executionWorldBundleFactory 等宿主可选注入不算默认。provider 的默认声明必须有从此根追到的装配证据；仅有接口或测试构造不能作默认构造点。

## 分母与状态

**总账为 101 项，不是 91 项。** 冻结 schema 的 `definitions.WsGatewayMethod.enum`、`gateway-methods.snapshot.json` 与研究 005 的 101 行清单集合完全相同，无缺项、额外项或重复项。分类为 **(a)10 + (b)46 + (c)4 + 待定41**。任务中的 91 是排除 A4 的 10 项后的剩余量；为满足全清单校验，本账本保留 A4 行，不更改冻结分类。

| 最终 method 交付 owner | 总数 | READY | BLOCKED |
|---|---:|---:|---:|
| A4 | 10 | 6 | 4 |
| A6 | 0 | 0 | 0 |
| A7 | 52 | 24 | 28 |
| A8 | 39 | 16 | 23 |
| 合计 | 101 | 46 | 55 |

`owner` 使用计划 005 的逻辑 issue 名；本任务未给出的 GitHub issue 数字不臆造。A6 建 provider，不独占 method；其前置关系在每行 `a6_dependencies` 中。A7 的 52 项是 C/S1–S8 共 50 项，再加 U9 `hook_async_result`、U10 `read_subagent_messages`；A8 总核验仍覆盖原待定41项，其中这两项复用 A7。

- **READY**：默认 CLI 静态路径、可选接口边界、至少一条成功/错误/未配置分支有可审阅证据，足以安排后续工作。不是 Rust 完成、所有配置分支通过或动态测试 PASS。
- **BLOCKED**：必需展开项仍缺证据。每行 `blockers` 明写 `missing` 与 `obtain`；已知的 native 路径仍保留，不因一条外部路径未知而清空整行。
- `branches` 区分成功、错误码/领域拒绝值、未配置。基线不存在某配置开关时显式 `N/A`，不发明 unavailable；例如 `resume_session` 只是返回输入，`run_get` 查不到返回 undefined。
- `notifications.names` 仅放 notification 帧名，`scope` 区分当前连接、广播和已核实的直接调用范围；`event_types` 单列 event 帧。BLOCKED 行的下游通知未知明确标注。
- `scenario_ids` 使用冻结 A2 driver 的组名，不捏造现有细分 ID。20 个 method 在已有组中被调用，81 个写 `NONE`；全部都有拟新增 `a51.<method>.<case>` 名称。`auth` 对 `describe_server` 只验握手前拒绝；`reconnect` 的 elicitation 回答断言为 `delivered:false`，均不冒充成功路径证据。拟新增场景尚未实现。

## 重点补证与未闭合边界

| 域 | 已回源确认 | 仍 BLOCKED / 怎么拿证据 |
|---|---|---|
| memory | `ProjectMemoryBundle` 选宿主 factory 或 native；native `EdgeClawMemoryService` 使用 `MemoryRepository`，`control.sqlite` + memory 文件目录；generation 释放最终调用 repository.close | 宿主 `memoryProviderFactory` 的实际生产实现；maintenance inFlight 与 generation dispose 的排空竞合。获取宿主装配/源码，以受控 maintenance + reload/dispose contract 验证 |
| snapshot | env 创建 `ContentAddressedWorkspaceSnapshotRecorder` 和文件 provider；内存 completed/inFlight，磁盘 objects/manifest/markers；restore 到新临时目录 | 三个管理方法静态 READY；实际文件失败/重启/隔离仍是拟新增场景，不能当作运行验证完成 |
| browser manager | `manager_browsers` 直接转接 `options.managerBrowsers`；源码搜索只找到转接与测试实例，未找到默认 CLI 生产实例 | 获取使用该宿主 option 的真实装配、状态介质和释放接口；不能拿测试返回列表冒充浏览器 provider |
| external binding | HTTP modules 的实际调用点、manifest 校验、credential 来源、timeout/abort/finally 清理已定位；session storage/upload/memory/agentLoop 各自列边界 | HTTP 适配器不是远端业务 provider。需要实际 deployment endpoint 的服务实现/manifest/生命周期；宿主注入需其真实 adapter。未知配置不能用统一 unavailable 删除 |

memory 证据链：`B/src/cli/ProjectRuntimeResourcesBundle.ts:387` → `B/src/cli/ProjectMemoryBundle.ts:57` → `B/src/context/memory/createEdgeClawMemoryProviderFromConfig.ts:54` → `B/src/context/memory/edgeclaw-memory-core/src/service.ts:624` → `B/src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts:522`。释放链见 `B/src/cli/ProjectRuntimeRegistry.ts:437`、`B/src/cli/ProjectRuntimeResourcesBundle.ts:430`、`B/src/cli/ProjectMemoryBundle.ts:82`、`B/src/context/memory/edgeclaw-memory-core/src/service.ts:655`。维护由 `B/src/cli/ProjectMemoryMaintenanceController.ts:70` 的异步循环执行；不据此宣称已证明退出时 drain。

snapshot 证据：`B/src/storage/workspaceSnapshot.ts:25`、`B/src/storage/workspaceSnapshot.ts:86`、`B/src/storage/workspaceSnapshot.ts:169`；宿主创建在 `B/src/cli/createLocalGateway.ts:611`、`B/src/cli/createLocalGateway.ts:615`、`B/src/cli/createLocalGateway.ts:618`。browser 证据：同文件 `B/src/cli/createLocalGateway.ts:232`、`B/src/cli/createLocalGateway.ts:1270`，测试注入 `B/tests/gateway/session-catalog-port.spec.ts:83`。完整引用和配置说明均存于账本，不复制基线实现。

## 更正

这些更正修复的是本账本的事实口径；**没有顺手修改冻结分类或旧研究清单**。

1. **91 与 101**：91 是非 A4 范围；101 才是全 method 分母。两份冻结产物和101清单相等，未发现来源间集合冲突。
2. **冻结脚本白名单的位置**：`check_frozen.py` 本体检查登记哈希；拒绝未登记 `.py/.mjs/.mts` 的逻辑实际在 `scripts/gateway-parity/common.py:44` 的 `apparatus_hashes()`（扫描与拒绝见 `scripts/gateway-parity/common.py:47`、`scripts/gateway-parity/common.py:55`）。因此独立使用 `specs/gateway-coverage/` 与 `scripts/gateway-coverage/`，不在旧脚本目录加文件，不登记新 hash，也不修改旧装置。
3. **snapshot 不是任意宿主 provider option**：`B/src/cli/createLocalGateway.ts:611` 读取 env，`B/src/cli/createLocalGateway.ts:615`、`B/src/cli/createLocalGateway.ts:618` 构造两个 native 实例。`B/src/storage/workspaceSnapshot.ts:28` 要求 `PILOTDECK_STORAGE_ROOT`，`B/src/storage/workspaceSnapshot.ts:32` 让 snapshot root 回退到它。不存在已证实的 YAML snapshot binding；restore 返回新目录，不是覆盖原 workspace（`B/src/cli/createLocalGateway.ts:1312`）。
4. **memory 不是“介质未知”或纯内存**：native 已追到 SQLite + memory 文件，默认根是 `~/.edgeclaw/memory`（`B/src/context/memory/edgeclaw-memory-core/src/service.ts:170`），不能假定在 `PILOT_HOME`。宿主 factory 与维护排空仍明确未知。
5. **browser 不借用 browser-use 工具提供者**：`managerBrowsers` 只有函数 option 转接。没有证据证明它自动接本地浏览器池，亦未见 browser disposer 注入（`B/src/cli/createLocalGateway.ts:232`、`B/src/cli/createLocalGateway.ts:1270`；`B/src/cli/LocalGatewayLifecycleBundle.ts:42`）。
6. **MCP 四个写入口不是全局 dirty/broadcast**：真实注入是 `B/src/cli/createLocalGateway.ts:1456`–`B/src/cli/createLocalGateway.ts:1459`，写 SDK Maps/permission rules 并关闭对应 session（`B/src/cli/ProjectRuntimeRegistry.ts:1216`、`B/src/cli/ProjectRuntimeRegistry.ts:1236`、`B/src/cli/ProjectRuntimeRegistry.ts:1278`、`B/src/cli/ProjectRuntimeRegistry.ts:1287`）。重连仅接受 SDK-owned enabled server；不把 `GatewayRuntimeRefreshBundle` 的全局广播归给这些方法。
7. **apply_flag_settings / reload_output_styles 不是统一 YAML 写入+广播**：前者走 SDK flags（`B/src/cli/ProjectRuntimeRegistry.ts:914`），后者刷新 plugin styles 并关相关 SDK sessions（`B/src/cli/ProjectRuntimeRegistry.ts:1111`）；这两条路径没有直接 `config_changed` 广播。真正全局更新链在 `B/src/cli/GatewayRuntimeRefreshBundle.ts:134`–`B/src/cli/GatewayRuntimeRefreshBundle.ts:163`，且 restart-only 与 refresh 失败不广播。
8. **list_sessions 主路径不是 Router Map**：实际注入在 `B/src/cli/createLocalGateway.ts:827`，到 `B/src/cli/ProjectRuntimeRegistry.ts:1703` 的 catalog consumer；仅无注入时才读 Map（`B/src/gateway/SessionRouter.ts:368`、`B/src/gateway/SessionRouter.ts:374`）。`manager_sessions` 同样走 catalog（`B/src/cli/createLocalGateway.ts:1263`）。冻结 (b) 不变。
9. **native archive / project listing 不能把默认文件路径说成所有配置**：artifact store 优先于 readFile（`B/src/cli/createLocalGateway.ts:1225`）；catalog 在 `B/src/cli/createLocalGateway.ts:685` 选宿主或 dataPlane，项目摘要也调用 catalog 计数（`B/src/web/server/listProjects.ts:181`）。有这些适配器边界不意味着改判冻结类别。
10. **model_catalog_list 的“纯函数”说法过强**：`B/src/model/session/NativeSessionModelSelectionPolicy.ts:22` 委派 `listModelCatalog`，后者调用 `loadPilotConfig`（`B/src/gateway/dialog/modelCatalog.ts:18`），会读取 YAML，而且 listCatalog 这条调用只传 env、不传 projectRoot；不能混同 resolveDefault 的项目配置路径（`B/src/model/session/NativeSessionModelSelectionPolicy.ts:46`）。它仍是冻结只读类。
11. **配置来源不只有 pilotdeck.yaml**：boot options/env 优先级见 `B/src/cli/LocalGatewayBootConfig.ts:43`；默认 YAML → 项目 YAML → env override 见 `B/src/pilot/config/loadPilotConfig.ts:80`、`B/src/pilot/config/loadPilotConfig.ts:94`、`B/src/pilot/config/loadPilotConfig.ts:105`。HTTP module credential 使用真实 `process.env`（`B/src/composition/HttpModuleClient.ts:43`），不能假定继承宿主 `options.env`。
12. **skills 的配置作用域有差别**：普通管理方法取传入 projectKey；validate/scan 使用默认项目（`B/src/cli/createLocalGateway.ts:868`、`B/src/cli/createLocalGateway.ts:869`）；disabled 抛 `SKILL_MODULE_DISABLED`（`B/src/cli/createLocalGateway.ts:2027`）。HTTP port 每次选择可新建，无服务级 dispose；请求 finally 仅清 timer/listener（`B/src/composition/HttpModuleClient.ts:131`）。
13. **通知与返回值不能混淆**：`permission_decide` 的 dispatch 返回后，WS 在 `B/src/gateway/server/GatewayWsConnection.ts:179` 先对当前连接发 `permission_changed`，再在 `B/src/gateway/server/GatewayWsConnection.ts:187` 发送 response；不检查 delivered；cron 的轮次回调使用 `always-on:turn-event`（`B/src/cli/pilotdeck.ts:85`），不是假定的 `cron_changed`。user-dialog 另走 server 广播（`B/src/cli/createLocalGateway.ts:1057`）。
14. **旧行号不能直接继承**：当前 `resolveSettings` 在 `B/src/gateway/client/InProcessGateway.ts:2359`（旧清单标到 `2411`，那里是 background-task guard）；claim/release 分别在 `B/src/gateway/client/InProcessGateway.ts:2734`、`B/src/gateway/client/InProcessGateway.ts:2765`。账本从当前基线重新定位所有 handler。

15. **run 读面每次刷新磁盘**：`FileRunRegistry.get/events` 等待 ready/mutationChain 后均执行 refreshFromDisk（`B/src/gateway/run/RunRegistry.ts:266`、`B/src/gateway/run/RunRegistry.ts:274`），不能描述成只读内存或一次 lazy load。
16. **upload lease 不是内存 pin/unpin**：`acquireAttachmentLease` 在项目 `.tmp/chat-upload-leases` 创建硬链接（`B/src/gateway/dialog/UploadStore.ts:255`、`B/src/gateway/dialog/UploadStore.ts:269`、`B/src/gateway/dialog/UploadStore.ts:342`），release 幂等删 leaseDir（`B/src/gateway/dialog/UploadStore.ts:288`）。consumer 获取失败反向释放已获取 lease（`B/src/cli/GatewayUploadedAttachmentBundle.ts:44`、`B/src/cli/GatewayUploadedAttachmentBundle.ts:72`）。

17. **S1/S3 逐入口防护不同**：只有 session model set/clear 调用 `reserveTranscriptWrite`（`B/src/gateway/client/InProcessGateway.ts:2193`、`B/src/gateway/client/InProcessGateway.ts:2203`）。restore 用 `restoringSessionKeys`、session/active-turn 与 transcriptExists 检查（`B/src/cli/createLocalGateway.ts:995`、`B/src/cli/createLocalGateway.ts:1315`、`B/src/cli/createLocalGateway.ts:1367`）；Set 的检查和 add 之间存在 await，不称为原子 reservation。export 只有 active-turn 检查；delete 先 close；rename/tag 分别检查 busy 再写 metadata。fork 无统一 reservation；read_session_messages 用三次 epoch 同步，replace/finalize 才进入 replacement 事务。
18. **领域码不能用 unavailable 代替**：restore 补 SESSION_BUSY / SESSION_RESTORE_CONFLICT / SESSION_RESTORE_UNSUPPORTED / INVALID_SESSION_ARCHIVE 等；export 补缺失、损坏、空 transcript 与 archive 校验；delete/rename/tag 补真实项目/会话码。默认 CLI 已注入这些 provider，IPG 的 CAPABILITY_UNAVAILABLE 仅对应自定义未注入装配。artifact 缺失实际抛出行是 `B/src/cli/createLocalGateway.ts:1228`（`B/src/cli/createLocalGateway.ts:1226` 是读取语句），已补 ARCHIVE_ARTIFACT_NOT_FOUND。
19. **三项 READY 经第3轮修正**：user_dialog_release 默认 bus 未命中后先解析项目，再因 store undefined 返回 released:false；FileGatewayUserDialogStore 只属宿主可选注入。background_tasks 调 registry.resolve，可能懒创建项目 runtime 并启动 watcher，也可能因配置或 disposed 报错。always_on_rerun_plan 的显式失败结果与无 catch 异常传播分别登记；只有已经进入 try 的 phase 才执行其 finally，可能残留 ready/executing 状态，跳过完成/history，workspace 不自动回收。修正后的 READY 仍只表示可安排下游夹具，不表示外部模型成功或动态时序通过。
20. **GLM 两项 P2**：fork 的 bundle 入口改为 `B/src/cli/GatewaySessionHistoryBundle.ts:150`；`B/src/cli/GatewaySessionHistoryBundle.ts:137` 是 readSubagentMessages。输入哈希加入 gap.md（工作包来源）与 plan 005（owner 权威），owner=A6 或错配 owner 均 FAIL。
21. **其余方法也回源**：上传、command catalog、模型选择、SDK settings、SOP/approval、skills HTTP/native 与 replacement 下游错误都按函数登记。源码中的动态 `errorByField` 映射人工展开；远端 response.code 与轮次 event.code 明记为动态集合，尖括号占位不是新的协议码。无业务错误的 resume_session/new_session/supported_agents/describe_server/active_turn_snapshot 不再凭运输兜底制造业务分支。

## 第 2 轮历史：扫描范围与方法（已由第3轮替代）

第2轮覆盖 **101/101 method**。其自报范围/hash与同名 inline provider 检查存在可绕过边界：缩范围并重算 hash、删 callee、加虚构码可 PASS。以下是当轮方法记录，不是当前校验器保证。第3轮已替换扫描器。每行的 `provider`、`state`、`guard` 是独立回源的说明；每个字段都有101个不同摘要，不使用工作包级模板。`source_audit.functions` 逐行列 handler/provider/callee 的**完整函数起止锚点、symbol、role、范围 sha256**，共1294个 method–函数关联、734个不同范围。起止同一行时只列一个锚点。分支记录共改动87个 method，其中76个新增错误/拒绝码记录，其余11个澄清普通异常与 string code 的运输区别；逐方法新增码与改动统计见 RESULT 第2轮及其 JSON 附件。

校验器只使用 Python 标准库，按以下顺序扫描：

1. 从冻结基线 WS switch 独立解析 method → InProcessGateway member，定位真实 handler 声明；从 createLocalGateway 的 InProcessGateway 初始化对象定位同名 inline provider。所有已定位声明的起点必须存在于该 method 的扫描范围，不能通过删除 restore provider 范围规避检查。property reference（例如 managerBrowsers option）不是函数体，实际实现由账本另列或 BLOCKED。
2. 读取账本所列完整函数范围，校验起止、角色、范围 hash，并验证所属文件的整体 hash。扫描 handler、直接 provider 与逐方法追踪的间接 callee；不把同文件其他 method 的错误笼统归到本 method。
3. 保留行偏移，屏蔽行/块注释（不把字符串中的 URL 当注释），扫描 `new DialogGatewayError`、`SkillManagerError`、`ForkSessionError`、`ReplaceLastTurnError`、`NodeProjectSessionReplacementError`、`PilotLocalSettingsError`、`PilotSdkSessionSettingsError`、`AlwaysOnError`、`RouterRuntimeError` 的首个字符串参数；同时扫描 `sopStateError/controlError/transportError` 首参、审批 `failure/reject` 的第二个字符串参数和直接 `code: "literal"`。
4. 扫到非 unconfigured 的领域码，而 error 分支只有 CAPABILITY_UNAVAILABLE / not_configured / SKILL_MODULE_DISABLED / unsupported（或 N/A/运输兜底），报 `provider_domain_errors`。此外，**只漏一个已扫描的码也报 `error_code_coverage`**，不允许补任意一个领域码敷衍通过。不会用前缀猜测所有 `_UNAVAILABLE` 都是未配置；例如 ROUTER_AUTO_UNAVAILABLE 仍是领域码。
5. 强制 owner：A4→A4；C、S1–S8、U9、U10→A7；其他 U→A8。A6 只能列在依赖字段。对每一处 prose 源码锚点逐个校验，包含同句中的第二/第三处；TS/JS 文件名也必须写成完整基线路径加正行号，裸文件名、无行号路径、零/越界/坏后缀均 FAIL。

扫描边界必须清楚：这是固定源码的有界词法检查，**不是 TypeScript 类型/控制流/全调用图证明**。间接 callee 及普通 Error 被 catch 后的转换由回源审阅决定；`errorByField[field]`、`error.code`、Node errno、SDK/外部 provider 与模型轮次的动态错误集合不能仅靠字面量扫描枚举。账本已人工补字段映射与后备码，明确标出动态传播和仍缺的具体宿主证据。未来源码改变会先由 file/range hash 拦截；扩充扫描范围与传播结论后才可更新账本。运行时状态、外部部署、真实网络和竞态仍交 A5.2/A6 场景，本轮没有启动服务。

## 第 3 轮：独立函数边界、默认装配与自动扫描

校验器从源码构建 AST 和静态调用图，不读取账本的终点/hash来决定扫描范围。账本的完整范围、hash、provider 起点、构造点与深度只作交叉校验；缩到首行并重算 hash 仍 FAIL。Python 驱动只用标准库，解析现在使用冻结基线**已有**的 Node/TypeScript 5.9.3；不下载依赖、不执行/import 产品代码、不启动服务，依赖缺失或解析失败即 FAIL。这一依赖变化替代了第2轮「纯 Python 标准库」保证。

1. **边界**：TypeScript parser 对函数声明、方法、箭头表达式、function expression 与 code-bearing Error 类求真实起止。字符串、模板插值、注释和正则字面量由 parser 处理；注释/正则中的伪括号与伪码不作为代码节点。source_audit 中自动扩展的函数按行压成一条完整 JSON 记录，便于审阅，数据未删减。
2. **装配**：从 WS case 独立解析 method→IPG member；解引用 import/局部变量、属性简写、对象/spread、三元与 ??/||、new 的具体类、构造参数/this 字段、工厂返回值。IPG 的构造参数只使用 createLocalGateway 的构造点，createLocalGateway 的 options 只使用 CLI 上述调用。运行时 setter 采用显式、固定的静态规则追 setAlwaysOnControl/setPrepareWeixinLogin/updateSubsystems/publishGeneration；接口签名不作 provider，不从账本猜实现。
3. **调用界限**：从 IPG handler 起固定 **8 跳**（账本不能改深度）。前两跳发现 provider 候选；余下层数覆盖 provider→registry→resolve/buildRuntime→resources.stage→loadPilotConfig→parser/validator 这类默认懒创建链。8 跳是静态扫描预算，不是全仓类型/控制流可达证明；回调与分支保守取并集，表达式解引用另有30节点循环护栏。到界限后不宣称所有远端/动态调用已闭合，仍交下游复核。账本另列的补充函数也独立求边界与扫字面量，删除自动发现 callee 会 FAIL；手工范围不能缩小自动扫描集合。
4. **默认 provider**：101项中97项自动发现 provider；4项校验器内白名单与账本理由必须精确一致：manager_browsers（默认无宿主注入）、resume_session（纯身份返回）、new_session（UUID，node:crypto builtin）、supported_agents（静态返回）。删除白名单、加任意跳过项或清空理由均 FAIL。default_boot/default_constructions/automatic_providers 与 provider.default_refs 由源码独立派生，伪造 FileGatewayUserDialogStore 默认构造点 FAIL；该文件补充范围统一 optional:true。每个范围另有源码核定的 assembly：automatic_default/code_definition/optional_host/supplemental_review；补充或可选范围伪称默认会 FAIL。
5. **领域码双向核验**：扫描所有声明了或继承 code 的 Error 类，按实际 constructor 参数位置识别；另扫描固定 code 字段/getter、this.code 赋值与条件/??/||后备字面量、super 的继承码、结构化 code 属性，以及 sopStateError/controlError/transportError/authorityError 与审批 failure/reject 工厂。扫到的码都须登记；登记的静态码都须在独立范围内有字面量。TOTALLY_FAKE_CODE FAIL。动态 error.code/response.code/event.code、Node errno，以及 N/A 无领域错误边界必须 dynamic:true 并提供 origin.refs 与该行真实 producing expression；缺位置、范围外位置或编造表达式均 FAIL。common WS handleRequest 是独立求界限的运输字面量/动态 origin 边界，不把整个 WS switch 的其他 method 领域码串到本项。
6. **语义护栏**：自动图包含 IPG.closeSession/router.close/onSessionEvict 时 state.effects 必须列出其授权、permission mode、reconnect owner/dialog key 与 agent 逐出副作用；含 registry.resolve 时 lifecycle.effects 必须登记可能懒创建 runtime/watcher。漏项分别报 close_state_effects/lazy_runtime_lifecycle。这些结构字段同时有源码锚点与中文说明，不能代替后续动态验证。

本轮 scope 扩张后，58项新增4045条 method–code 登记（207个不同 code，含2个显式动态边界）；新增项使用 `observed: lexical` 标识，说明是函数扫描观察，**内部 catch/转换、条件分支与 WS 可达性必须再复核**。不把配置检查/cron 内部事件全部声称为 RPC 顶层抛码。background_tasks 已包含 CONFIG_INVALID_VALUE、CONFIG_MEMORY_*、CONFIG_AGENT_SUBAGENTS_INVALID 等默认懒创建失败；update_settings 覆盖写盘后 reload 的配置失败。逐方法完整增补清单见 RESULT「第3轮」与其 round3-changes JSON。

### Error 类清单

全仓自有源码 grep 找到65条 `class …Error` 记录；排除3条生成/声明副本与非 Error 子类的 ToolRendererErrorBoundary 后，独立 AST 清单有61个 Error 类，其中39个声明或继承 code。所有61项均写入 ledger.error_classes 并校验源码位置、继承关系、code 标记与参数位置；换新类名不会落出手写正则。下表逐个列出39项。参数位置从1开始；「字段/对象/继承」通过类完整补充范围、构造函数与 code 属性规则识别，动态值保留 origin。

| Error 类 | code 来源规则 | 声明锚点 |
|---|---|---|
| `ModelConfigError` | constructor 第1参 | `B/src/model/protocol/errors.ts:154` |
| `ModelRequestError` | constructor 第1参 | `B/src/model/protocol/errors.ts:166` |
| `NetworkFetchError` | constructor 第1参 | `B/src/network/fetch.ts:15` |
| `SandboxUnavailableError` | 字段/对象参数/继承 super（动态或固定码） | `B/src/tool/execution-world/SandboxPort.ts:54` |
| `AgentRuntimeError` | constructor 第1参 | `B/src/agent/protocol/errors.ts:25` |
| `LspError` | constructor 第2参 | `B/src/lsp/protocol/errors.ts:11` |
| `PilotDeckToolRuntimeError` | constructor 第1参 | `B/src/tool/protocol/errors.ts:27` |
| `PilotDeckLifecycleRuntimeError` | constructor 第1参 | `B/src/lifecycle/protocol/errors.ts:1` |
| `PilotSdkSessionSettingsError` | constructor 第1参 | `B/src/pilot/config/sdkSessionSettings.ts:67` |
| `PilotLocalSettingsError` | constructor 第1参 | `B/src/pilot/config/updateLocalSettings.ts:89` |
| `PilotConfigError` | constructor 第1参 | `B/src/pilot/config/types.ts:312` |
| `RouterConfigError` | constructor 第1参 | `B/src/router/protocol/errors.ts:1` |
| `RouterRuntimeError` | constructor 第1参 | `B/src/router/protocol/errors.ts:13` |
| `SessionEventValidationError` | constructor 第1参 | `B/src/session/events/SessionEventLogValidation.ts:12` |
| `SessionDomainValidationError` | constructor 第1参 | `B/src/session/events/SessionDomainValidation.ts:29` |
| `NodeProjectSessionReplacementError` | constructor 第1参 | `B/src/session/replacement/NodeProjectSessionReplacementPort.ts:46` |
| `AgentLoopResultUnknownError` | 字段/对象参数/继承 super（动态或固定码） | `B/src/agent/modules/transport/operationLedger.ts:77` |
| `McpClientError` | constructor 第2参 | `B/src/mcp/client/McpClient.ts:61` |
| `DialogGatewayError` | constructor 第1参 | `B/src/gateway/dialog/errors.ts:1` |
| `SkillManagerError` | constructor 第1参 | `B/src/extension/skills/SkillManager.ts:489` |
| `SkillValidationError` | 字段/对象参数/继承 super（动态或固定码） | `B/src/extension/skills/SkillManager.ts:501` |
| `RunRegistryError` | constructor 第1参 | `B/src/gateway/run/RunRegistry.ts:34` |
| `GatewayRequestError` | constructor 第1参 | `B/src/gateway/client/GatewayWsClient.ts:20` |
| `HttpStoreValidationError` | constructor 第1参 | `B/src/gateway/user-dialog/HttpGatewayUserDialogStore.ts:571` |
| `AlwaysOnError` | constructor 第1参 | `B/src/always-on/protocol/errors.ts:13` |
| `ForkSessionError` | constructor 第1参 | `B/src/web/server/forkSession.ts:331` |
| `ReplaceLastTurnError` | constructor 第1参 | `B/src/web/server/replaceLastTurn.ts:48` |
| `SessionExecutionProviderError` | constructor 第2参 | `B/src/sandbox/SessionExecutionProvider.ts:88` |
| `StaffDeckSopClientError` | constructor 第1参 | `B/src/sop/staffdeck/StaffDeckSopClient.ts:23` |
| `StaffDeckSopDiscoveryClientError` | constructor 第1参 | `B/src/sop/staffdeck/StaffDeckSopDiscoveryClient.ts:5` |
| `SessionStoreError` | constructor 第1参 | `B/packages/sdk/src/session-store.ts:26` |
| `PilotDeckError` | 字段/对象参数/继承 super（动态或固定码） | `B/packages/sdk/src/types.ts:1165` |
| `AbortError` | 字段/对象参数/继承 super（动态或固定码） | `B/packages/sdk/src/types.ts:1186` |
| `ApprovalAuthorityError` | constructor 第2参 | `B/ui/server/staffdeck-approval-authority.mjs:2` |
| `PublicKnowledgeModelError` | constructor 第1参 | `B/ui/server/public-knowledge-model-selection.mjs:4` |
| `CopyError` | constructor 第2参 | `B/ui/server/routes/staffdeck-copy.js:7` |
| `PublicCapabilityError` | constructor 第1参 | `B/ui/server/staffdeck-public-capabilities.mjs:3` |
| `ApiError` | 字段/对象参数/继承 super（动态或固定码） | `B/ui/src/composition/modules/staffdeck/vendor/FormalApiError.ts:2` |
| `SpreadsheetProtocolError` | constructor 第2参 | `B/skills/spreadsheets/scripts/spreadsheet_cli.mjs:70` |

不带 code 的22类也受清单漂移检查；其 message/name 不作为领域码：`ModelProviderError`、`StreamIdleTimeoutError`、`IncompleteStreamError`、`MaxStreamingDurationError`、`RepeatedStreamingChunkError`、`BackgroundTaskSnapshotRecoveryError`、`BackgroundTaskStateUnknownError`、`WebFetchHttpError`、`GoalConflictError`、`TokenSaverTimeoutError`、`ProjectSessionCatalogUnavailableError`、`ProjectSessionForkUnavailableError`、`ProjectSessionReplacementUnavailableError`、`ProjectSessionSearchUnavailableError`、`SidecarInstanceRestartedError`、`SidecarAbortError`、`CompactionPersistenceError`、`WorkspaceBoundaryError`、`MemoryBundleValidationError`、`InvalidWorkflowDefinitionError`、`WorkflowRunBindingError`、`RestartTimeoutError`。

### 第3轮已核语义更正

- user_dialog_respond：默认 journal 位于 <transcriptPath>.dialogs.json，恢复写 transcript；注入 FileGatewayUserDialogStore 另列。live bus 的 lease_required 与 store lease 不符 false 后可 fallback/recover 分开；补 ephemeral/in-flight/session queue/purpose 去重，四种返回形态，PROJECT_NOT_FOUND 先于 peek，以及 consume 后 acknowledge 抛错时 pending 已丢失。A6.8 拟夹具必须断言这些基线行为。
- user_dialog_release：默认 store undefined 的 released:false 路径、expired lease 即使 release=false 也可能通知；宿主文件 store 的 open(wx)/rm 文件锁据实登记。注入异常单列 dynamic_boundary。
- mcp_server_reconnect：补 IPG.closeSession 删除会话授权、mode、reconnect owner/dialog key 与 router agent 逐出；success 锚点1290；CAPABILITY_UNAVAILABLE 仅 unconfigured。
- update_settings：文件已改但 reload 失败 snapshot 未换；补配置码、mkdtemp try 外、临时 writeFile 包装、本方法 normalize 前置拦截、无更新锁、reload 合并，以及 SDK config hook/broadcast 锚点。
- session_model_clear：reservation 最早可拒绝，随后项目→sessionKey→active turn；补共用写锁。state 改成关闭/逐出与清 mode，保留会话授权；缺 projectKey 的 Node ERR_INVALID_ARG_TYPE 为动态码；修 WS 锚点223/251。
- background_tasks：可能创建/缓存 runtime/watcher，配置失败保留原码，disposed 普通异常走 WS 兜底。always_on_rerun_plan：补默认 native plan/state/cycle/report 与 workspace tool 审计；无 catch 的 phase 失败、状态残留、未执行 completion/history 与未回收 workspace；复用/legacy 跳过 cwd safety；workspace_unavailable 与 workspace_prepare_failed 分清。
- elicitation_respond 的 delivered:false 移到成功/拒绝返回说明，error=[]；补 bus 与 reconnect 的双 binding 匹配。并修正 permission_decide/grant_session_permission 同类的非 code 返回值；project_file_read 改为默认 CLI inline provider，IPG 内建路径只属 fallback，new_session 的默认 uuid 明确为 node:crypto。

## 验证与维护

```sh
python3 scripts/gateway-coverage/check_ledger.py
python3 scripts/gateway-parity/check_frozen.py
git diff --check
```

校验器仍核三方 method 集合、冻结类别、owner、字段、每处锚点、引用文件与输入哈希、A2场景调用。默认从 checkout/祖先寻找只读 resources/PilotDeck，可显式 --baseline。每次验证执行38项内存负例，必须命中指定错误类型；另有6项共用生产规则的解析/改名 Error 夹具。第3轮新增缩范围+重算hash+删callee、删除callee、虚构码、改成原正则未覆盖的新code类/未定义类、白名单篡改、缺关闭副作用/懒创建、可选store伪称默认与缺动态origin。独立CLI副本负例的退出码另存 RESULT 附件。

机器保证结构与扫描完整性；不证明每段中文语义、动态竞态或远端部署。下游每个 PR 前复核是规划索引的必要维护步骤。没有运行 A2/A5 服务对拍；READY/BLOCKED 仍为46/55。原始结果与完整新码清单在 `_tmp/RESULT.md` 第3轮。

## 第4轮：去重后的账本格式

`format_version=2` 只改变存储结构。以第3轮完整文件为参照，展开后所有字段、说明、锚点、动态 origin、数组顺序逐项相同；仍为101项、46 READY / 55 BLOCKED。账本由7,814,247字节降到1,453,876字节；去重未改变扫描的保守并集，也未把 lexical 观察升级为 WS 顶层错误。

- `ranges` 按源文件分表，表内每个稳定 id 对应 `start/end/symbol/sha256`。文件名即该范围的 `file`；SHA256仍为完整函数范围的原值。method 的 `source_audit.functions` 存连续同角色关联的 id 列表、`role/assembly/optional`；连续段的顺序保留原扫描顺序。`provider.default_refs` 和 `automatic_providers` 同样引用范围 id，展开为该范围起点。
- `error_code_groups` 根据错误记录锚点所在的最内层已登记 AST 函数范围自动分组。只有完整记录一致、产生函数集合一致、使用 method 集合一致的重复 lexical 记录才成组；没有按 CONFIG_/TOOLS_ 等码前缀手工归类。组名来自产生函数名，多个产生函数时带 `chain`；`source_ranges` 给出全部产生位置和函数入口。保留本方法独立说明的记录放在 `specific`。
- `branches.error.groups` 引用组；`specific` 放其余完整记录。展开时先按引用顺序拼接各组，再接 `specific`；`order` 是此临时列表的下标排列，精确恢复第3轮错误分支的原顺序，不删码、不合并不同说明。
- 重复构造点、审计说明也分别放入 `construction_points`、`audit_texts`。`error_summaries` 存从原说明逐字提取的前缀/后缀，`summary.template` 在展开时插入该记录的 `code`，得到原说明；没有改写文案。
- 所有对象键稳定排序；嵌套缩进为2空格。完整证据对象、范围行、角色关联段和标量列表各占一行。id 由原内容或范围位置的 SHA256 前32位编码得到，生成器检查冲突，冲突会失败；不随方法遍历次序编号。

转换工具为 `scripts/gateway-coverage/ledger_format.py`。下游 PR 可先展开到 `_tmp/` 修改受影响记录，再重新提取表与组；转换器在写出前要求无损往返。对账本直接改表或引用也可，但必须通过同一校验器。

```sh
python3 scripts/gateway-coverage/ledger_format.py --expand specs/gateway-coverage/ledger.json --output _tmp/ledger-expanded.json
python3 scripts/gateway-coverage/ledger_format.py --compact _tmp/ledger-expanded.json --output specs/gateway-coverage/ledger.json
python3 scripts/gateway-coverage/check_ledger.py
```

`check_ledger.py` 在执行第3轮的全部检查前展开引用，并拒绝不存在的组/范围、组来源冲突、虚构组内码、重复 JSON 键、遗漏或重复的 `order` 下标等。一次性第3轮对照用 `--compare-expanded _tmp/ledger-round3.json`；默认日常校验不依赖本地快照。完整结果与负例见 `_tmp/RESULT.md` 的「第4轮」。本轮未发现须另列的第3轮语义错误。

## 第 5 轮：校验器剩余结构缺口（2026-10-14）

动态分支现在同时核验 **code 形态和 origin**。`dynamic:true` 仅接受非空、无空白的 `<…>` 占位、`N/A`、`ERR_[A-Z0-9]+(?:_[A-Z0-9]+)*`；普通大写常量必须走静态字面量核验，不能借 dynamic 逃逸。`origin.expression` 必须逐字等于所引源码行去首尾空白后的全文，或等于同一个已扫描范围中实际产码 AST 节点的源码，且 origin 行落在该节点跨度内。AST 节点来自 code 属性/字段/getter/赋值、带 code 的 Error 构造参数、继承 super 参数及已解析工厂的 code 参数；单字符子串、任意括号与不在 origin 行的表达式均 FAIL。

默认装配根严格取 `B/src/cli/pilotdeck.ts:107`–`B/src/cli/pilotdeck.ts:116`，不取该文件稍后的其他 `createLocalGateway` 调用。旧 `default_boot` 误取 `B/src/cli/pilotdeck.ts:347` 的 fallback，本轮修正。装配分析沿真实调用、传入/返回的具体回调、已构造实例的构造函数/字段初始化，保守展开条件分支；CLI 构造并返回的 InProcessGateway RPC handler 是运行时入口，继续追其懒创建路径。构造 site 与函数绑定迭代到有限图的不动点；构造参数只解引用已从此根发现的 sites。全 src 存在 `new`、其他 factory 的注入或没有被调用的函数中的 `new` 均不能成为默认构造证据。默认可达范围禁止 `optional:true`；默认图外的 provider 范围须标 `optional_host` + `optional:true`，保留其补充证据。

反向字面量池和产码观察统一使用 `ast_rules.cjs:errorCodeLiteral`，只接受：

- 大写下划线形式：`[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+`，如 `PROJECT_DELETED`；
- 小写下划线形式：`[a-z][a-z0-9]*(?:_[a-z0-9]+)+`，如 `gateway_request_failed`；
- 当前 Node `node:os.constants.errno` 的真实 key，如 `ETIMEDOUT`（不把任意 E 开头英文词当 errno）；
- 冻结源码已明确使用的单词码有限集合：`aborted/conflict/internal/required/terminal/timeout/unauthorized/unavailable/unsupported`。依据分别可查 `B/packages/sdk/src/types.ts:1188`、`B/src/extension/skills/SkillManager.ts:278`、`B/src/always-on/runtime/DiscoveryScheduler.ts:165`、`B/src/tool/execution/validateToolInput.ts:64`、`B/src/gateway/run/RunRegistry.ts:243`、`B/packages/sdk/src/transport.ts:143`、`B/src/adapters/web/httpRouter.ts:34`、`B/src/tool/registry/createBuiltinRegistry.ts:160`、`B/src/tool/builtin/executeCode.ts:410`。

普通词、typeof/type 名（如 object/string/function/number/boolean/undefined/Object/Error）与类名 AgentLoop 不构成错误码证据。形态规则只决定本账本接受的码字面量，仍要求在该 method 的独立范围/共同运输边界中出现；不会因全仓有同名常量而接受。由此删除 9 条把 `B/src/pilot/staffdeck/projectBinding.ts:49`、`B/src/pilot/staffdeck/projectBinding.ts:51` 的消息文本误记成码的 lexical 记录，没有补登该工厂的其他错误码。字段映射、捕获/转换与实际传播仍交下游复核。

### 8 跳截断后的下游复核名单

默认必需扫描仍为 **8 跳**。报告的 `beyond_depth_review` 另沿同一静态调用边遍历到有限图闭合，**只生成复核名单，不把后续范围/码加入账本、必需范围或 PASS 判据**。跳数为 handler 到函数的最短调用边距离；同一未登记码多处出现时只取其最短距离，表中是这些距离的集合。以下 47 个 method 在第 **9** 跳即有未登记码，仅列 method 与跳数；不补登、不推断为 WS 顶层抛码。

| method | 首个未登记码的最短跳数 | 未登记码的最短跳数集合 |
|---|---:|---|
| `background_task_stop` | 9 | 9, 10, 11, 12 |
| `background_tasks` | 9 | 9, 10, 11, 12 |
| `commands_list` | 9 | 9, 10, 11, 12, 13 |
| `delete_session` | 9 | 9, 10, 11, 12 |
| `export_session_transcript` | 9 | 9, 10, 11, 12 |
| `finalize_last_turn_replacement` | 9 | 9, 10, 11, 12, 13, 14, 15, 16 |
| `fork_session` | 9 | 9, 10, 11, 12, 13, 14 |
| `mcp_server_status` | 9 | 9, 10, 11, 12 |
| `memory_list` | 9 | 9, 10, 11 |
| `memory_wipe` | 9 | 9, 10, 11 |
| `model_catalog_list` | 9 | 9, 10 |
| `model_usage_snapshot` | 9 | 9, 10, 11, 12 |
| `native_archive_artifact` | 9 | 9, 10, 11, 12 |
| `native_archive_entries` | 9 | 9, 10, 11, 12, 13 |
| `native_archive_manifest` | 9 | 9, 10, 11, 12, 13 |
| `output_styles_list` | 9 | 9, 10, 11, 12 |
| `read_session_messages` | 9 | 9, 10, 11, 12, 13, 14, 15 |
| `read_subagent_messages` | 9 | 9, 10, 11, 12, 13, 14, 15 |
| `record_agent_status_message` | 9 | 9, 10, 11, 12, 13, 14, 15, 16 |
| `reload_config` | 9 | 9 |
| `reload_extensions` | 9 | 9, 10, 11 |
| `reload_output_styles` | 9 | 9, 10, 11, 12, 18, 19 |
| `rename_session` | 9 | 9, 10, 11, 12 |
| `replace_last_turn` | 9 | 9, 10, 11, 12, 13, 14, 15, 16 |
| `restore_session_transcript` | 9 | 9, 10, 11, 12 |
| `rewind_files` | 9 | 9, 10, 11, 12 |
| `seed_read_state` | 9 | 9, 10, 11, 12, 13, 15, 16, 17, 18, 19, 20, 21, 23 |
| `session_model_get` | 9 | 9, 10 |
| `session_model_set` | 9 | 9, 10 |
| `set_mcp_servers` | 9 | 9, 10, 11, 12, 15, 16 |
| `set_output_style` | 9 | 9, 10, 11, 12, 18, 19 |
| `skill_create` | 9 | 9, 10, 11, 12 |
| `skill_delete` | 9 | 9, 10, 11, 12 |
| `skill_import` | 9 | 9, 10, 11, 12 |
| `skill_list` | 9 | 9, 10, 11, 12 |
| `skill_read` | 9 | 9, 10, 11, 12 |
| `skill_scan` | 9 | 9, 10, 11, 12 |
| `skill_validate` | 9 | 9, 10, 11, 12 |
| `skill_write` | 9 | 9, 10, 11, 12 |
| `sop_resume` | 9 | 9, 10, 11, 12, 13, 14 |
| `sop_status` | 9 | 9, 10, 11, 12, 13, 14 |
| `steer_turn` | 9 | 9, 12, 13 |
| `submit_turn` | 9 | 9, 10, 11, 12, 13, 15 |
| `tag_session` | 9 | 9, 10, 11, 12 |
| `usage_snapshot` | 9 | 9, 10, 11, 12 |
| `user_dialog_list` | 9 | 9, 10, 11, 12, 13, 14 |
| `user_dialog_respond` | 9 | 9, 10, 11, 12, 13 |

继续检查第 10 跳以后，还观察到以下 10 项较晚才出现缺口；与上述 47 项分列，以免把「第9跳边界」误称为全图完备。

| method | 首个跳数 | 跳数集合 |
|---|---:|---|
| `apply_flag_settings` | 15 | 15, 16 |
| `cancel_steer` | 12 | 12, 13 |
| `close_project_sessions` | 16 | 16, 17 |
| `close_session` | 15 | 15, 16 |
| `describe_server` | 16 | 16, 17 |
| `hook_async_result` | 11 | 11, 14, 15 |
| `mcp_server_reconnect` | 15 | 15, 16 |
| `mcp_server_toggle` | 15 | 15, 16 |
| `set_mcp_permission_mode_override` | 15 | 15, 16 |
| `set_session_thinking` | 15 | 15, 16 |

本轮账本仍为 101 项、46 READY / 55 BLOCKED，method–范围关联仍 16,816；仅必要的默认 metadata、optional 标记、消息误记及失去引用的 source_files 改动。完整逐项差异、对第3轮的严格展开对照与验证命令见 `_tmp/RESULT.md`「第5轮」及其 JSON 附件。GLM 第3轮已给出「无 P0/P1」，其三项 P2 留作下游复核。
