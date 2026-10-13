# 冻结清单 · 101 个 gateway method 全分类

> 2026-10-14。分母以本表为准：`resources/PilotDeck/src/gateway/protocol/frames.ts:21-122` 逐行数得 **101** 个 method（`WsGatewayMethod` 联合类型），本表每个 method 一行、逐一定分类、每行带 `路径:行号` 依据。行号相对 `resources/PilotDeck/`（基线 @ `cc7c4e6`，本 worktree 逐条回源核对；分发处行号缩写 **GWC** = `src/gateway/server/GatewayWsConnection.ts`，**IPG** = `src/gateway/client/InProcessGateway.ts`）。
> 本表是对 exp002「(a) 约 35 / (b) 约 45 / (c) 5」口径的**重审与冻结**（exp002 的名单未入仓，分母可动 → 第 1 轮评审判 P0）。重审口径见下。

## 分类口径（本轮收紧）

- **(a) 无状态只读**：实现只读**盘上状态**（`PILOT_HOME` 下文件 / 项目目录 / 静态常量），不读任何进程内运行时状态。**读内存运行时状态的不算 (a)**——凡经 `SessionRouter` 内存 Map、`turnEventCoordinator` / `turnReplacementCoordinator`、进程级 bus（permission / user dialog）、`ProjectRuntimeRegistry` 的各运行时（router.stats、sdkSessionPlugins、backgroundSubagents…）、`FileRunRegistry` 内存表、`CronManager` runtimes、`configStore` 内存快照、`hasActiveTurn` 检查——一律降级。
- **(b) 会话域**：行为耦合到 TS 进程内的会话/轮次/交互运行时状态（含对它们的写）。
- **(c) 依赖连接状态**：分发处注入或校验 `interactionBinding`。
- **待定**：写全局状态（settings/reload/mcp/cron/skill 写、upload 生命周期、广播 `config_changed` 那批）与存储介质未逐行核的读——**一律透传**，归属等对应状态整体迁移时再定。

计数：**(a) 10 · (b) 46 · (c) 4 · 待定 41 = 101**（第 2 轮评审后 5 个 method 由 (a) 改待定，协调者 2026-10-14 改）。（exp002 的「(c) 5 个」把帧类型 `hello` 计入了 method；101 枚举里 `hello` 不是 method，故此处 (c) = 4。）

与 exp002 口径相比的两个代表改判（评审指名）：
- `read_session_messages`：只读盘会漏在飞事件——IPG 合并 `turnEventCoordinator` 内存轮次快照并校验 epoch（`IPG:2850-2860`），**(a) → (b)**。
- `export_session_transcript`：读盘前先查 `router.hasActiveTurn`（`createLocalGateway.ts:1165-1167`），会因内存运行态返回 `SESSION_BUSY`，**(a) → (b)**。

## 清单（按 `frames.ts:21-122` 原序）

| # | method | 分类 | 依据（分发 → 实现） | 备注 |
|---|---|---|---|---|
| 1 | `submit_turn` | **(c)** | GWC:123-137（注入 binding `:133`、事件流）→ IPG:764 | (b) 亦成立（轮次运行时）；分发注入 binding 故记 (c) |
| 2 | `upload_create` | 待定·写 | GWC:261 → IPG:2483-2489 | uploadLifecycle port（`createLocalGateway.ts:1051` `dialog.uploads`），介质未核 |
| 3 | `upload_get` | 待定 | GWC:264 → IPG:2491-2493 | 同上，读经 port |
| 4 | `upload_part` | 待定·写 | GWC:267 → IPG:2495-2510 | |
| 5 | `upload_complete` | 待定·写 | GWC:270 → IPG:2512-2514 | |
| 6 | `upload_cancel` | 待定·写 | GWC:273 → IPG:2516-2518 | |
| 7 | `run_get` | (b) | GWC:282 → IPG:2525-2529 → `src/gateway/run/RunRegistry.ts:54-57, 70-77` | `FileRunRegistry` 内存 records/eventMap + ownerPid 存活检查 |
| 8 | `run_events` | (b) | GWC:285 → IPG:2531-2549 → 同上 | |
| 9 | `run_reattach` | (b) | GWC:288 → IPG:2551-2553 → 同上 | `runGet` 同款 |
| 10 | `permission_list` | (b) | GWC:291 → IPG:2780-2792 | `getPermissionBus().snapshot()`，进程级 bus 内存 |
| 11 | `memory_list` | 待定 | GWC:294 → IPG:2794-2799 → `createLocalGateway.ts:1238-1247` → `src/cli/ProjectMemoryBundle.ts:47-74` | EdgeClaw live 服务实例持有状态，存储介质未核 |
| 12 | `memory_wipe` | 待定·写 | GWC:297 → IPG:2800-2808 | |
| 13 | `snapshot_list` | 待定 | GWC:300 → IPG:2809-2814 → `createLocalGateway.ts:1271-1300` | workspaceSnapshotProvider，介质未核 |
| 14 | `snapshot_get` | 待定 | GWC:303 → IPG:2815-2820 → 同上 | |
| 15 | `snapshot_restore` | 待定·写 | GWC:306 → IPG:2821-2827 | |
| 16 | `manager_sessions` | 待定 | GWC:309 → IPG:2828-2831 → `createLocalGateway.ts:1261-1268` | `registry.listSessions`，未核 |
| 17 | `manager_browsers` | 待定 | GWC:312 → IPG:2832-2835 | 外部注入 port（`createLocalGateway.ts:1270`） |
| 18 | `native_archive_manifest` | **(a)** | GWC:315 → IPG:2555-2560 → `createLocalGateway.ts:1183-1196` | 读盘 native transcript 后统计 |
| 19 | `native_archive_entries` | **(a)** | GWC:318 → IPG:2562-2573 → `createLocalGateway.ts:1197-1216` | 读盘分页 |
| 20 | `native_archive_artifact` | **(a)** | GWC:321 → IPG:2575-2587 → `createLocalGateway.ts:1215-1237` | `readFileAsync` 读工具产物 |
| 21 | `steer_turn` | (b) | GWC:276 → IPG:1859 | 轮次运行时 |
| 22 | `cancel_steer` | (b) | GWC:278 → IPG:1886 | |
| 23 | `abort_turn` | (b) | GWC:280 → IPG:2000 | |
| 24 | `list_sessions` | (b) | GWC:324 → IPG:2022-2024 → `src/gateway/SessionRouter.ts:368-373`（`sessions` 内存 Map `:78`） | |
| 25 | `resume_session` | (b) | GWC:326 → IPG:2026-2028 | 当前实现为恒等返回；语义属会话域，随运行时整体迁移 |
| 26 | `sop_status` | (b) | GWC:328 → IPG:634-637 → `createLocalGateway.ts:1072-1080` | `sopControl.status` 内存 |
| 27 | `sop_resume` | (b) | GWC:331 → IPG:639-652 | `hasActiveTurn` 检查（`:642`） |
| 28 | `new_session` | (b) | GWC:334 → IPG:2030-2034 | 生成会话键（当前不落盘），语义是创建 |
| 29 | `close_session` | (b)·写 | GWC:339 → IPG:2049-2054 | router.close + coordinator + 内存清理 |
| 30 | `delete_session` | (b)·写 | GWC:341 → IPG:2229-2239 | |
| 31 | `export_session_transcript` | (b) | GWC:344 → IPG:2241-2248 → `createLocalGateway.ts:1162-1180` | **改判**：`hasActiveTurn` 内存检查 `:1165-1167` |
| 32 | `restore_session_transcript` | (b)·写 | GWC:347 → IPG:2250-2257 | |
| 33 | `rename_session` | (b)·写 | GWC:350 → IPG:2211-2218 | |
| 34 | `tag_session` | (b)·写 | GWC:353 → IPG:2220-2227 | |
| 35 | `close_project_sessions` | (b)·写 | GWC:336 → IPG:2036-2047 | |
| 36 | `record_agent_status_message` | (b) | GWC:356 → IPG:2068-2078 | live router 分支在前 |
| 37 | `describe_server` | 待定 | GWC:361 → IPG:2088-2114 | 能力清单来自运行时注入 options；**决定：保持透传**（`gap.md` 3.4） |
| 38 | `project_files_list` | **(a)** | GWC:363 → IPG:2165-2174 → `src/gateway/dialog/projectFiles.ts:29+` | 盘上目录遍历 + 项目注册校验 |
| 39 | `commands_list` | 待定 | GWC:365 → IPG:2176-2179 → `src/cli/GatewayCommandCatalogBundle.ts:46-58` | `pluginRuntime` lease（内存）+ `.pilotdeck/commands` 盘扫混合 |
| 40 | `model_catalog_list` | **(a)** | GWC:367 → IPG:2181-2184 → `src/model/session/NativeSessionModelSelectionPolicy.ts:21-24` | `listModelCatalog(input, env)`，配置/env 的纯函数 |
| 41 | `session_model_get` | **(a)** | GWC:369 → IPG:2186-2189 → `src/model/session/NativeSessionModelSelectionPort.ts:51-55` | 读盘 session persistence + `replayTranscriptEntries` |
| 42 | `session_model_set` | (b)·写 | GWC:371 → IPG:2191-2199 | `turnReplacementCoordinator` 写锁 + 写盘 |
| 43 | `session_model_clear` | (b)·写 | GWC:373 → IPG:2201-2209 | 同上 |
| 44 | `active_turn_snapshot` | (b) | GWC:375 → IPG:2479-2481 | `turnEventCoordinator.snapshot`，纯内存 |
| 45 | `mcp_server_status` | (b) | GWC:384 → IPG:2279-2282 → `src/cli/ProjectRuntimeRegistry.ts:1293-1310` | `sdkSessionPlugins`/`sdkSessionMcpServers` 内存 |
| 46 | `project_file_read` | **(a)** | GWC:399 → IPG:2259-2277 | `readFile` + 路径越界校验，纯盘 |
| 47 | `set_mcp_servers` | 待定·写 | GWC:387 → IPG:2284-2292 | markAllDirty + 广播（`GatewayRuntimeRefreshBundle.ts:144, 159-160`） |
| 48 | `mcp_server_reconnect` | 待定·写 | GWC:390 → IPG:2293-2300 | |
| 49 | `mcp_server_toggle` | 待定·写 | GWC:393 → IPG:2301-2308 | |
| 50 | `set_mcp_permission_mode_override` | 待定·写 | GWC:396 → IPG:2309-2320 | |
| 51 | `set_permission_mode` | (b)·写 | GWC:402 → IPG:2321-2330 | `permissionModes` 内存 Map（`:2327`）+ `hasActiveTurn`（`:2325`） |
| 52 | `apply_flag_settings` | 待定·写 | GWC:405 → IPG:2337-2349 | 写全局 |
| 53 | `update_settings` | 待定·写 | GWC:408 → IPG:2350-2363 | 写全局 + 广播 |
| 54 | `resolve_settings` | 待定 | GWC:411 → IPG:2411-2413 → `createLocalGateway.ts:1402` | `configStore.getSnapshot()` 内存快照；Rust 直读盘需先证「快照=盘」 |
| 55 | `set_session_thinking` | (b)·写 | GWC:414 → IPG:2364-2373 | 会话设置 |
| 56 | `output_styles_list` | (b) | GWC:417 → IPG:2374-2377 → `src/cli/ProjectRuntimeRegistry.ts:1075-1092` | `pluginRuntime.refresh` + `sdkSessionPlugins`/`sdkSessionConfigs` 内存 |
| 57 | `set_output_style` | (b)·写 | GWC:420 → IPG:2379-2385 | `hasActiveTurn`（`:2382`） |
| 58 | `reload_output_styles` | 待定·写 | GWC:423 → IPG:2387-2390 | 写全局 + 广播 |
| 59 | `usage_snapshot` | (b) | GWC:426 → IPG:2392-2395 → `src/cli/ProjectRuntimeRegistry.ts:1310-1347` | `router.stats` 内存聚合 |
| 60 | `model_usage_snapshot` | (b) | GWC:429 → IPG:2397-2400 → 同上 `:1348+` | |
| 61 | `rewind_files` | (b)·写 | GWC:432 → IPG:2402-2408 | `hasActiveTurn`（`:2406`） |
| 62 | `background_task_stop` | (b)·写 | GWC:435 → IPG:2410-2415 | 运行时写 |
| 63 | `background_tasks` | (b) | GWC:438 → IPG:2417-2421 → `src/cli/ProjectRuntimeRegistry.ts:1729-1753` | `backgroundSubagents`/`executionWorld` 内存 |
| 64 | `hook_async_result` | 待定 | GWC:441 → IPG:1896+（`submitAsyncHookResult`） | wiring 未逐行核 |
| 65 | `seed_read_state` | (b)·写 | GWC:444 → IPG:2423-2442 | `router.seedReadState` + `setSdkSessionConfig`（`:2434-2435`） |
| 66 | `supported_agents` | **(a)** | GWC:447 → IPG:2444-2454 | `SUBAGENT_DEFINITIONS` 编译期常量，不读任何状态 |
| 67 | `reconnect_interaction` | **(c)** | GWC:450-463（注入 `nextBinding` `:460`）→ IPG:672 | 重连绑定换轨 |
| 68 | `cron_create` | 待定·写 | GWC:464 → IPG:2603-2605 | cron 运行时 + 广播 |
| 69 | `cron_list` | 待定 | GWC:466 → IPG:2607-2609 → `src/cron/runtime/CronManager.ts:121-129`（`runtimes` 内存 Map `:52`） | |
| 70 | `cron_update` | 待定·写 | GWC:468 → IPG:2611-2613 | |
| 71 | `cron_delete` | 待定·写 | GWC:470 → IPG:2615-2617 | |
| 72 | `cron_stop` | 待定·写 | GWC:472 → IPG:2619-2621 | |
| 73 | `cron_run_now` | 待定·写 | GWC:474 → IPG:2623-2625 | |
| 74 | `elicitation_respond` | **(c)** | GWC:476-480（注入 binding `:479`）→ IPG:2627-2636 | |
| 75 | `user_dialog_list` | (b) | GWC:481 → IPG:2710-2727 | `userDialogBus.list` 内存（`:2714`）+ hosted/recovered |
| 76 | `user_dialog_claim` | (b) | GWC:484 → IPG:2729-2768 | `userDialogBus.claim`（`:2750`），跨进程租约 |
| 77 | `user_dialog_release` | (b) | GWC:487 → IPG:2770-2774 | `userDialogBus.release`（`:2769-2771`） |
| 78 | `user_dialog_respond` | (b) | GWC:490 → IPG:2638-2708 | `userDialogBus.consumeForResponse`（`:2680`） |
| 79 | `permission_decide` | **(c)** | GWC:492-506（binding 注入 `:502-505`、sdk 豁免分支 `:500-501`）→ IPG:2776-2778 | bus 在进程级（`gap.md` 3.3），透传最安全 |
| 80 | `grant_session_permission` | (b) | GWC:507 → IPG:2837-2839 | `interactionCoordinator` |
| 81 | `read_session_messages` | (b) | GWC:509 → IPG:2841-2861 | **改判**：合并 `turnEventCoordinator` 内存轮次快照并校验 epoch（`:2850-2860`） |
| 82 | `read_subagent_messages` | **待定** | GWC:511 → IPG:2863-2870（纯透传）→ `src/cli/GatewaySessionHistoryBundle.ts:107` → `src/web/server/readSessionMessages.ts:263+` | 盘上 subagent transcript；在飞时会读到半行，读实现须容忍截断 | （第 2 轮评审：`read_subagent_messages` 先 `recoverTranscriptReplacements()`，可能写盘——`GatewaySessionHistoryBundle.ts:137-141`）
| 83 | `fork_session` | (b)·写 | GWC:513 → IPG:2872-2879 | 会话复制 |
| 84 | `replace_last_turn` | (b) | GWC:515 → IPG:2881-2883 | `turnReplacementCoordinator` |
| 85 | `finalize_last_turn_replacement` | (b) | GWC:517 → IPG:2885-2889 | 同上 |
| 86 | `list_projects` | **(a)** | GWC:519 → IPG:2891-2896 → `createLocalGateway.ts:1488` → `src/web/server/listProjects.ts:2-31` | 真相源 = `$PILOT_HOME/projects` 目录 |
| 87 | `describe_project` | **(a)** | GWC:521 → IPG:2898-2903 → `createLocalGateway.ts:1489` → `src/cli/GatewayDialogBundle.ts:79-84` | 同一 projects 目录 |
| 88 | `reload_config` | 待定·写 | GWC:523 → IPG:2905-2910 | 写全局 + 广播 |
| 89 | `prepare_weixin_login` | 待定 | GWC:528 → IPG:2912-2921 | 外部副作用（微信登录） |
| 90 | `reload_extensions` | 待定·写 | GWC:537 → IPG:2923-2928 | 写全局 + 广播 |
| 91 | `skill_list` | **待定** | GWC:542 → IPG:2950-2952 | SkillManager 盘操作（IPG:2942-2948 注释） | （第 2 轮评审：skill 读方法先读缓存配置再选禁用 / 外部后端——`createLocalGateway.ts:854`；两家分歧取保守）
| 92 | `skill_read` | **待定** | GWC:544 → IPG:2954-2956 | | （第 2 轮评审：skill 读方法先读缓存配置再选禁用 / 外部后端——`createLocalGateway.ts:854`；两家分歧取保守）
| 93 | `skill_write` | 待定·写 | GWC:546 → IPG:2958-2960 | 写全局 |
| 94 | `skill_create` | 待定·写 | GWC:548 → IPG:2962-2964 | |
| 95 | `skill_delete` | 待定·写 | GWC:550 → IPG:2966-2968 | |
| 96 | `skill_import` | 待定·写 | GWC:552 → IPG:2970-2972 | |
| 97 | `skill_validate` | **待定** | GWC:554 → IPG:2974-2976 | 只读校验 | （第 2 轮评审：skill 读方法先读缓存配置再选禁用 / 外部后端——`createLocalGateway.ts:854`；两家分歧取保守）
| 98 | `skill_scan` | **待定** | GWC:556 → IPG:2978-2980 | 只读扫描 | （第 2 轮评审：skill 读方法先读缓存配置再选禁用 / 外部后端——`createLocalGateway.ts:854`；两家分歧取保守）
| 99 | `always_on_apply` | (b) | GWC:558 → IPG:2992-2998 | `alwaysOnControl` 运行时端口 |
| 100 | `always_on_abort` | (b) | GWC:563 → IPG:2999-3009 | |
| 101 | `always_on_rerun_plan` | (b) | GWC:568 → IPG:3010+ | |

## (a) 类小结（10 个 = A4 本地化的分母；第 2 轮评审后由 15 降为 10，协调者改）

`native_archive_manifest` · `native_archive_entries` · `native_archive_artifact` · `project_files_list` · `project_file_read` · `model_catalog_list` · `session_model_get` · `supported_agents` · `list_projects` · `describe_project`

（`describe_server` 虽只读，但读的是运行时注入 options，且与 `hello_ok.serverInfo` 同源——**保持透传**，`gap.md` 3.4，不入 (a)。）

## 冻结与维护

- 本表冻结后即为计划 002 指标的**唯一分母**；改分类 = 改分母，须人过目并记变更，不得在实现过程中顺手改。
- (b)(c) 的行号依据是「现状」而非「永久归属」：会话运行时整体迁移（计划 003）时，(b) 类随之整块迁走，本表只做透传清单用。
- 「待定」41 个在各自状态域迁移前一律透传；其中存储介质未核的（upload/memory/snapshot/manager/hook），透传期顺带补核，结论回填本表。
