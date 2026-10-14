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

## 分母与状态

**总账为 101 项，不是 91 项。** 冻结 schema 的 `definitions.WsGatewayMethod.enum`、`gateway-methods.snapshot.json` 与研究 005 的 101 行清单集合完全相同，无缺项、额外项或重复项。分类为 **(a)10 + (b)46 + (c)4 + 待定41**。任务中的 91 是排除 A4 的 10 项后的剩余量；为满足全清单校验，本账本保留 A4 行，不更改冻结分类。

| 最终 method 交付 owner | 总数 | READY | BLOCKED |
|---|---:|---:|---:|
| A4 | 10 | 6 | 4 |
| A6 | 0 | 0 | 0 |
| A7 | 52 | 21 | 31 |
| A8 | 39 | 16 | 23 |
| 合计 | 101 | 43 | 58 |

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

snapshot 证据：`B/src/storage/workspaceSnapshot.ts:25`、`:86`、`:169`；宿主创建在 `B/src/cli/createLocalGateway.ts:611`、`:615`、`:618`。browser 证据：同文件 `:232`、`:1270`，测试注入 `B/tests/gateway/session-catalog-port.spec.ts:83`。完整引用和配置说明均存于账本，不复制基线实现。

## 更正

这些更正修复的是本账本的事实口径；**没有顺手修改冻结分类或旧研究清单**。

1. **91 与 101**：91 是非 A4 范围；101 才是全 method 分母。两份冻结产物和101清单相等，未发现来源间集合冲突。
2. **冻结脚本白名单的位置**：`check_frozen.py` 本体检查登记哈希；拒绝未登记 `.py/.mjs/.mts` 的逻辑实际在 `scripts/gateway-parity/common.py:44` 的 `apparatus_hashes()`（扫描与拒绝见 `:47`、`:55`）。因此独立使用 `specs/gateway-coverage/` 与 `scripts/gateway-coverage/`，不在旧脚本目录加文件，不登记新 hash，也不修改旧装置。
3. **snapshot 不是任意宿主 provider option**：`createLocalGateway.ts:611` 读取 env，`:615`、`:618` 构造两个 native 实例。`workspaceSnapshot.ts:28` 要求 `PILOTDECK_STORAGE_ROOT`，`:32` 让 snapshot root 回退到它。不存在已证实的 YAML snapshot binding；restore 返回新目录，不是覆盖原 workspace（`createLocalGateway.ts:1312`）。
4. **memory 不是“介质未知”或纯内存**：native 已追到 SQLite + memory 文件，默认根是 `~/.edgeclaw/memory`（`service.ts:170`），不能假定在 `PILOT_HOME`。宿主 factory 与维护排空仍明确未知。
5. **browser 不借用 browser-use 工具提供者**：`managerBrowsers` 只有函数 option 转接。没有证据证明它自动接本地浏览器池，亦未见 browser disposer 注入（`createLocalGateway.ts:232`、`:1270`；`LocalGatewayLifecycleBundle.ts:42`）。
6. **MCP 四个写入口不是全局 dirty/broadcast**：真实注入是 `createLocalGateway.ts:1456`–`:1459`，写 SDK Maps/permission rules 并关闭对应 session（`ProjectRuntimeRegistry.ts:1216`、`:1236`、`:1278`、`:1287`）。重连仅接受 SDK-owned enabled server；不把 `GatewayRuntimeRefreshBundle` 的全局广播归给这些方法。
7. **apply_flag_settings / reload_output_styles 不是统一 YAML 写入+广播**：前者走 SDK flags（`ProjectRuntimeRegistry.ts:914`），后者刷新 plugin styles 并关相关 SDK sessions（`:1111`）；这两条路径没有直接 `config_changed` 广播。真正全局更新链在 `GatewayRuntimeRefreshBundle.ts:134`–`:163`，且 restart-only 与 refresh 失败不广播。
8. **list_sessions 主路径不是 Router Map**：实际注入在 `createLocalGateway.ts:827`，到 `ProjectRuntimeRegistry.ts:1703` 的 catalog consumer；仅无注入时才读 Map（`SessionRouter.ts:368`、`:374`）。`manager_sessions` 同样走 catalog（`createLocalGateway.ts:1263`）。冻结 (b) 不变。
9. **native archive / project listing 不能把默认文件路径说成所有配置**：artifact store 优先于 readFile（`createLocalGateway.ts:1225`）；catalog 在 `:685` 选宿主或 dataPlane，项目摘要也调用 catalog 计数（`web/server/listProjects.ts:181`）。有这些适配器边界不意味着改判冻结类别。
10. **model_catalog_list 的“纯函数”说法过强**：`NativeSessionModelSelectionPolicy.ts:22` 委派 `listModelCatalog`，后者调用 `loadPilotConfig`（`gateway/dialog/modelCatalog.ts:18`），会读取 YAML，而且 listCatalog 这条调用只传 env、不传 projectRoot；不能混同 resolveDefault 的项目配置路径（`NativeSessionModelSelectionPolicy.ts:46`）。它仍是冻结只读类。
11. **配置来源不只有 pilotdeck.yaml**：boot options/env 优先级见 `LocalGatewayBootConfig.ts:43`；默认 YAML → 项目 YAML → env override 见 `loadPilotConfig.ts:80`、`:94`、`:105`。HTTP module credential 使用真实 `process.env`（`HttpModuleClient.ts:43`），不能假定继承宿主 `options.env`。
12. **skills 的配置作用域有差别**：普通管理方法取传入 projectKey；validate/scan 使用默认项目（`createLocalGateway.ts:868`、`:869`）；disabled 抛 `SKILL_MODULE_DISABLED`（`:2027`）。HTTP port 每次选择可新建，无服务级 dispose；请求 finally 仅清 timer/listener（`HttpModuleClient.ts:131`）。
13. **通知与返回值不能混淆**：`permission_decide` 在返回后对当前连接发 `permission_changed`，并不检查 delivered（`GatewayWsConnection.ts:176`）；cron 的轮次回调使用 `always-on:turn-event`（`pilotdeck.ts:85`），不是假定的 `cron_changed`。user-dialog 另走 server 广播（`createLocalGateway.ts:1057`）。
14. **旧行号不能直接继承**：当前 `resolveSettings` 在 `InProcessGateway.ts:2359`（旧清单标到 `2411`，那里是 background-task guard）；claim/release 分别在 `:2734`、`:2765`。账本从当前基线重新定位所有 handler。

15. **run 读面每次刷新磁盘**：`FileRunRegistry.get/events` 等待 ready/mutationChain 后均执行 refreshFromDisk（`B/src/gateway/run/RunRegistry.ts:266`、`:274`），不能描述成只读内存或一次 lazy load。
16. **upload lease 不是内存 pin/unpin**：`acquireAttachmentLease` 在项目 `.tmp/chat-upload-leases` 创建硬链接（`B/src/gateway/dialog/UploadStore.ts:255`、`:269`、`:342`），release 幂等删 leaseDir（`:288`）。consumer 获取失败反向释放已获取 lease（`B/src/cli/GatewayUploadedAttachmentBundle.ts:44`、`:72`）。

## 验证与维护

```sh
python3 scripts/gateway-coverage/check_ledger.py
python3 scripts/gateway-parity/check_frozen.py
git diff --check
```

校验器只用标准库，默认从 checkout 及祖先目录寻找 `resources/PilotDeck`，也可传 `--baseline /path/to/PilotDeck`。缺少基线或源码漂移均 FAIL，不降级跳过。它验证三方 method 集合、重复、冻结类别、工作包、必填字段、每个源码锚点边界、引用文件哈希及冻结协议源哈希，并检查场景确实在对应 A2 driver 分支中调用该 method。JSON 输出含 READY/BLOCKED、owner 与引用统计。

每次正常验证同时对内存副本做负例：删除 method、重复 method、行号越界、缺 owner；另加缺场景、改类别、虚构场景，共7项。负例必须因预定错误类型 FAIL，原账本不写回。

机器只能核实锚点存在与输入未漂移，不能代替“此行支持此结论”的源码审阅。动态交错、外部部署行为和全量成功路径仍按 BLOCKED/拟新增场景处理；没有运行 A2/A5 对拍或服务。A6 估算与本次验证原始结果放在任务交付 `_tmp/RESULT.md`。
