# 路由与 WS 消息清单 · 阶段 B 的分母

> 2026-10-14。被查 `resources/PilotDeck` @ `cc7c4e6`。由 6 路只读子 agent 分文件盘点，主会话逐条核对行号与挂载前缀；**给不出行号的行不写**。
> 行号相对 `resources/PilotDeck/`。口径见文末「口径与已知遮蔽」。

## 汇总（分母）

| 项 | 数 | 说明 |
|---|---|---|
| HTTP 路由注册 | **264** | 非测试文件里方法注册的**逐条清点**（含 4 条 `router.route('/x').get().post()` 链式路由；其中 `routes/update.js:112` 未挂载，**可用 263**）。**这只是静态清单；分母以 §10 的运行时枚举为准**。任务书里的「约 280」应还含 `.use` 挂载与工厂注册 |
| 挂载前缀组 | **23** | `/api` 下 22 组（`index.js:540-648`）+ `/api/always-on`、`/api/ccr`、`/api/agents`、`/api/browse-filesystem` 等直注册在 `index.js` 的散路由 |
| WS 端点 | **3** | `/ws`、`/shell`、`/plugin-ws/*`（`index.js:2299-2304` 分发） |
| WS 入站消息 type | **26** | `/ws` 23 种 + `/shell` 3 种；无类型 JSON，见下 WS 节 |
| SSE / 流式端点 | **5 组** | `index.js:1033`、`routes/projects.js:539`、`routes/uploads.js:137`、`routes/agent.js:828`（stream 形态）、`routes/modules.js:449-450`；另有 NDJSON `routes/webUpdate.js:12` |

**每文件路由注册数**（合计 264；方法计数脚本会把 `router.route(...)` 链式与 `restartRouter.post` 漏掉，故本清单逐条清点起步，**最终以 §10 运行时枚举对账为准**）：

| 文件 | 数 | 文件 | 数 |
|---|---|---|---|
| `ui/server/index.js` | 52 | `routes/memory.js` | 27 |
| `routes/git.js` | 20 | `routes/config.js` | 20 |
| `routes/taskmaster.js` | 16 | `routes/gateway.js` | 16 |
| `routes/settings.js` | 15 | `routes/modules.js` | 13 |
| `routes/skills.js` | 11 | `routes/projects.js` | 11 |
| `routes/mcp.js` | 10 | `routes/plugins.js` | 8 |
| `routes/uploads.js` | 7 | `routes/user.js` | 6 |
| `routes/auth.js` | 5 | `routes/onboarding.js` | 5 |
| `routes/webUpdate.js` | 5 | `routes/sop.js` | 4 |
| `routes/commands.js` | 3 | `routes/messages.js` | 3 |
| `routes/mcp-utils.js` | 2 | `routes/update.js` | 2（1 条未挂载） |
| `routes/agent.js` | 1 | `routes/models.js` | 1 |
| `routes/staffdeck-copy.js` | 1 | `routes/cron-jobs.js` | **0**（只导出 handler，`index.js:752` 用） |

**列读法**：`读写磁盘` = 该 handler 主效应是否读/写盘（含 spawn 子进程的 git/CLI 写）；`调 src/` = 该文件**顶层直接** import `src/` 且该符号在 handler 里被使用（传递依赖另注）；`WS 广播依赖` = 是否使用 UI 端的 `connectedClients` / `broadcast*` / `process.emit('pilotdeck:config-broadcast')`（与插件 gateway 的 WS RPC 不算广播）。

---

## 1. `ui/server/index.js` 本体（52 条 + WS/SSE）

> `src/` 列：本文件无直接 `src/` import；网关能力经 `./pilotdeck-bridge.js`（`index.js:73-99`）转 `../../src/gateway/index.js:60`，项目数据经 `./projects.js`（`index.js:72`）转 `src/`。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| USE | `*`（全局） | ui/server/index.js:512 | — | — | — | CORS，暴露 `X-Refreshed-Token` |
| USE | `*`（全局） | ui/server/index.js:513 | — | — | — | express.json 50mb，跳过 multipart |
| USE | `*`（全局） | ui/server/index.js:524 | — | — | — | express.urlencoded 50mb |
| GET | `/health` | ui/server/index.js:527 | — | — | — | 公开健康检查，无鉴权 |
| USE | `/api` | ui/server/index.js:540 | — | — | — | `validateApiKey` 中间件 |
| USE | `/api/auth` | ui/server/index.js:543 | — | — | — | 挂载 routes/auth.js |
| GET | `/api/projects/files` | ui/server/index.js:547 | — | 是（:86→bridge:60） | — | gateway.projectFilesList，须先于 :589 |
| USE | `/api/projects` | ui/server/index.js:589 | — | — | — | routes/projects.js |
| USE | `/api/git` | ui/server/index.js:592 | — | — | — | routes/git.js |
| USE | `/api/mcp` | ui/server/index.js:595 | — | — | — | routes/mcp.js |
| USE | `/api/taskmaster` | ui/server/index.js:598 | — | — | — | routes/taskmaster.js |
| USE | `/api/memory` | ui/server/index.js:601 | — | — | — | routes/memory.js |
| USE | `/api/mcp-utils` | ui/server/index.js:604 | — | — | — | routes/mcp-utils.js |
| USE | `/api/commands` | ui/server/index.js:607 | — | — | — | routes/commands.js |
| USE | `/api/skills` | ui/server/index.js:612 | — | — | — | routes/skills.js |
| USE | `/api/uploads` | ui/server/index.js:613 | — | — | — | routes/uploads.js |
| USE | `/api/models` | ui/server/index.js:614 | — | — | — | routes/models.js |
| USE | `/api/settings` | ui/server/index.js:617 | — | — | — | routes/settings.js |
| USE | `/api/config` | ui/server/index.js:620 | — | — | — | routes/config.js |
| USE | `/api/v1` | ui/server/index.js:624 | — | — | — | routes/onboarding.js |
| USE | `/api/gateway` | ui/server/index.js:627 | — | — | — | routes/gateway.js |
| USE | `/api/user` | ui/server/index.js:630 | — | — | — | routes/user.js |
| USE | `/api/plugins` | ui/server/index.js:633 | — | — | — | routes/plugins.js |
| GET | `/api/sessions/model` | ui/server/index.js:637 | — | — | — | handler 来自 routes/models.js |
| PUT | `/api/sessions/model` | ui/server/index.js:638 | — | — | — | 同上 |
| DELETE | `/api/sessions/model` | ui/server/index.js:639 | — | — | — | 同上 |
| USE | `/api/sessions` | ui/server/index.js:640 | — | — | — | routes/messages.js |
| USE | `/api/sop` | ui/server/index.js:641 | — | — | — | routes/sop.js |
| USE | `/api/modules` | ui/server/index.js:642 | — | — | — | `createModuleRuntimeRouter` |
| USE | `/api/agent` | ui/server/index.js:645 | — | — | — | routes/agent.js（API key 鉴权） |
| USE | `/api/update` | ui/server/index.js:648 | — | — | — | routes/update.js |
| GET | `/api/agents/runtime-config` | ui/server/index.js:653 | 读（:654→permissionSettings.js:57） | — | — | 静态 stub |
| USE | `/api/cursor`、`/api/codex`、`/api/gemini`、`/api/cli` | ui/server/index.js:669 | — | — | — | 循环注册 4 前缀，一律 410 |
| GET | `/api/ccr/dashboard` | ui/server/index.js:681 | 读（:683→bridge ~:2954） | — | — | 读 router stats.json(l) |
| GET | `/api/always-on/events` | ui/server/index.js:690 | 读（:694→always-on-events.js:17/63） | — | — | 事件文件 |
| GET | `/api/always-on/cron-jobs` | ui/server/index.js:705 | — | 是（:707→gateway.cronList） | — | |
| POST | `/api/always-on/cron-jobs` | ui/server/index.js:715 | 写 | 是（:737-738） | — | gateway.cronCreate |
| PATCH | `/api/always-on/cron-jobs/:taskId` | ui/server/index.js:752 | 写 | 是（:752） | — | routes/cron-jobs.js |
| POST | `/api/always-on/cron-jobs/:taskId/run-now` | ui/server/index.js:754 | — | 是（:756-757） | — | gateway.cronRunNow |
| POST | `/api/always-on/cron-jobs/:taskId/stop` | ui/server/index.js:768 | — | 是（:770-771） | — | gateway.cronStop |
| DELETE | `/api/always-on/cron-jobs/:taskId` | ui/server/index.js:782 | 写 | 是（:784-785） | — | gateway.cronDelete |
| GET | `/api/ccr/health` | ui/server/index.js:797 | — | — | — | 常量响应 |
| GET | `/api/ccr/config` | ui/server/index.js:807 | — | — | — | 返回 null |
| GET | `/api/ccr/stats/summary` | ui/server/index.js:815 | 读（:817） | — | — | 读 router stats |
| GET | `/api/ccr/stats/sessions/:sessionId` | ui/server/index.js:823 | 读（:825） | — | — | 读 router stats |
| POST | `/api/ccr/stats/reset` | ui/server/index.js:835 | — | — | — | 501 未实现 |
| PUT | `/api/ccr/config` | ui/server/index.js:845 | — | — | — | 501，配置归 /api/config |
| GET | `/memory-dashboard` | ui/server/index.js:852 | 读（:854, :858） | — | — | 需鉴权 |
| USE | `/memory-dashboard` | ui/server/index.js:861 | 读（express.static） | — | — | 静态资源 |
| USE | `/memory-dashboard` | ui/server/index.js:876 | — | — | — | 兜底 404 |
| USE | `*`（public） | ui/server/index.js:881 | 读（express.static） | — | — | ui/public |
| USE | `*`（dist） | ui/server/index.js:885 | 读（express.static） | — | — | ui/dist，HTML 禁缓存 |
| GET | `/api/projects` | ui/server/index.js:905 | 读（:907） | 是（:72→projects.js:100） | 是（:907→:284 `connectedClients`） | 与 :589 挂载重叠 |
| GET | `/api/projects/:projectName/sessions` | ui/server/index.js:923 | 读（:926-927 sqlite） | 是（:72→projects.js:495） | — | |
| PUT | `/api/projects/:projectName/rename` | ui/server/index.js:943 | 写（:946） | — | — | `renameProject` 实为 no-op（`ui/server/projects.js:537`） |
| DELETE | `/api/projects/:projectName/sessions/:sessionId` | ui/server/index.js:954 | 写（:958→`ui/server/projects.js:568` fs.unlink、:576 fs.rm） | — | 是（:966 `connectedClients`） | 广播 `session-deleted` |
| PUT | `/api/sessions/:sessionId/rename` | ui/server/index.js:978 | 写（:995 sqlite） | — | — | |
| DELETE | `/api/projects/:projectName` | ui/server/index.js:1004 | 写（:1008→`ui/server/projects.js:593` fs.rm） | — | — | force 可选 |
| POST | `/api/projects/create` | ui/server/index.js:1016 | 写（:1024） | 是（:1024→`ui/server/projects.js`） | — | |
| SSE | `/api/search/conversations` | ui/server/index.js:1033（`text/event-stream` :1043） | 读（:1054） | 是（:72→gateway.listProjects） | — | `app.get` 注册；推 `result/progress/done/error` |
| GET | `/api/browse-filesystem` | ui/server/index.js:1362 | 读（:1394-1404） | — | — | |
| POST | `/api/browse-filesystem/native-folder` | ui/server/index.js:1436 | — | — | — | 原生目录选择对话框 |
| POST | `/api/create-folder` | ui/server/index.js:1454 | 写（:1480 mkdir） | — | — | 先校验父目录 |
| GET | `/api/projects/:projectName/file` | ui/server/index.js:1495 | 读（:1519 readFile） | — | — | `requireRealProjectFilesystem` |
| GET | `/api/projects/:projectName/files/content` | ui/server/index.js:1534 | 读（:1555, :1562, :1564） | — | — | 支持 Range/HEAD/download |
| GET | `/api/office-preview/status` | ui/server/index.js:1577 | 读（:1581-1583） | — | — | 限流 60/min |
| GET | `/api/projects/:projectName/files/preview/pdf` | ui/server/index.js:1609 | 读写（:1648-1649） | — | — | LibreOffice，限流 30/min |
| GET | `/api/projects/:projectName/files/preview/spreadsheet/manifest` | ui/server/index.js:1672 | 读写（:1703） | — | — | |
| GET | `/api/projects/:projectName/files/preview/spreadsheet/data` | ui/server/index.js:1715 | 读写（:1747） | — | — | |
| GET | `/api/projects/:projectName/files/preview/spreadsheet/sheet` | ui/server/index.js:1764 | 读写（:1794） | — | — | |
| GET | `/api/projects/:projectName/preview/*` | ui/server/index.js:1817 | 读（:1833-1845） | — | — | 相对路径资源 |
| GET | `/api/projects/:projectName/download` | ui/server/index.js:1853 | 读（:1867） | — | — | zip 流 |
| PUT | `/api/projects/:projectName/file` | ui/server/index.js:1896 | 写（:1925 writeFile） | — | — | |
| GET | `/api/projects/:projectName/files` | ui/server/index.js:1944 | 读（:1966） | — | — | 深度 10，隐藏 dot 目录 |
| POST | `/api/projects/:projectName/files/create` | ui/server/index.js:2021 | 写（:2066, :2075） | — | — | |
| PUT | `/api/projects/:projectName/files/rename` | ui/server/index.js:2098 | 写（:2151 rename） | — | — | |
| DELETE | `/api/projects/:projectName/files` | ui/server/index.js:2175 | 写（:2214, :2216） | — | — | 禁止删项目根 |
| POST | `/api/projects/:projectName/files/upload/check` | ui/server/index.js:2241 | 读（`workspaceFileUpload.js`） | — | — | `createWorkspaceUploadCheckHandler` |
| POST | `/api/projects/:projectName/files/upload` | ui/server/index.js:2244 | 写（`workspaceFileUpload.js`） | — | — | `createWorkspaceFileUploadHandler` |
| POST | `/api/projects/:projectName/upload-attachments` | ui/server/index.js:3093 | 读写（:3080-3082, :3101, :3147, :3167） | — | — | multer 10×20MB，图片转 dataURL |
| POST | `/api/projects/:projectName/upload-images` | ui/server/index.js:3183 | 读写（:3194, :3237, :3242） | — | — | multer 5×5MB |
| GET | `/api/projects/:projectName/sessions/:sessionId/token-usage` | ui/server/index.js:3268 | 读（:3315, :3340, :3403） | 是（:3277←bridge） | — | pilotdeck 走 bridge；codex/其他读 jsonl |
| USE | `/api` | ui/server/index.js:3461 | — | — | — | API 404 兜底，防落 SPA |
| GET | `*` | ui/server/index.js:3471 | 读（:3483, :3488） | — | — | SPA；无 dist 则重定向 Vite |

## 2. `routes/git.js`（20）+ `routes/taskmaster.js`（16）

挂载：`/api/git`（`index.js:592`）、`/api/taskmaster`（`index.js:598`）。两文件顶层无 `src/` import；`extractProjectDirectory`（`../projects.js`）与 `runChatViaGateway`（`../pilotdeck-bridge.js`）是传递依赖。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| GET | /api/git/status | ui/server/routes/git.js:291 | 读（git status:308） | — | — | modified/added/deleted/untracked |
| GET | /api/git/diff | ui/server/routes/git.js:354 | 读（fs.readFile:391） | — | — | 单文件 diff；未跟踪直读盘 |
| GET | /api/git/file-with-diff | ui/server/routes/git.js:437 | 读（fs.readFile:486） | — | — | 新旧内容，供 CodeEditor |
| POST | /api/git/initial-commit | ui/server/routes/git.js:517 | 写（git add:539, commit:542） | — | — | 已有 HEAD 拒绝 |
| POST | /api/git/commit | ui/server/routes/git.js:561 | 写（git add:578, commit:582） | — | — | |
| POST | /api/git/revert-local-commit | ui/server/routes/git.js:592 | 写（git reset:614, update-ref:625） | — | — | soft reset |
| GET | /api/git/branches | ui/server/routes/git.js:639 | 读（git branch -a:653） | — | — | 本地+远程 |
| POST | /api/git/checkout | ui/server/routes/git.js:683 | 写（git checkout:695） | — | — | |
| POST | /api/git/create-branch | ui/server/routes/git.js:705 | 写（git checkout -b:717） | — | — | |
| POST | /api/git/delete-branch | ui/server/routes/git.js:727 | 写（git branch -d:744） | — | — | 禁止删当前分支 |
| GET | /api/git/commits | ui/server/routes/git.js:753 | 读（git log:771, show:793） | — | — | limit 默认 10、上限 100 |
| GET | /api/git/commit-diff | ui/server/routes/git.js:810 | 读（git show:825） | — | — | 超 500k 截断 |
| POST | /api/git/generate-commit-message | ui/server/routes/git.js:842 | 读（git diff:865, fs.readFile:886） | — | — | AI 生成；本地 writer 收集器（:937-972）非 WS |
| GET | /api/git/remote-status | ui/server/routes/git.js:1043 | 读（git rev-parse:1083, rev-list:1098） | — | — | ahead/behind |
| POST | /api/git/fetch | ui/server/routes/git.js:1121 | 写（git fetch:1145） | — | — | |
| POST | /api/git/pull | ui/server/routes/git.js:1162 | 写（git pull:1190） | — | — | |
| POST | /api/git/push | ui/server/routes/git.js:1230 | 写（git push:1258） | — | — | |
| POST | /api/git/publish | ui/server/routes/git.js:1301 | 写（git push --set-upstream:1343） | — | — | |
| POST | /api/git/discard | ui/server/routes/git.js:1380 | 写（fs.rm/unlink:1414/1416；git restore:1420） | — | — | |
| POST | /api/git/delete-untracked | ui/server/routes/git.js:1434 | 写（fs.rm/unlink:1472/1475） | — | — | 仅未跟踪 |
| GET | /api/taskmaster/installation-status | ui/server/routes/taskmaster.js:249 | 读（mcp-detector.js:31） | — | — | CLI 探测（:41/:60）+读 `~/.pilotdeck` |
| GET | /api/taskmaster/detect/:projectName | ui/server/routes/taskmaster.js:284 | 读（taskmaster.js:120/164） | — | — | |
| GET | /api/taskmaster/detect-all | ui/server/routes/taskmaster.js:356 | 读（taskmaster.js:120/164） | — | — | |
| POST | /api/taskmaster/initialize/:projectName | ui/server/routes/taskmaster.js:440 | — | — | — | 占位，恒 501 |
| GET | /api/taskmaster/next/:projectName | ui/server/routes/taskmaster.js:466 | 读（CLI:483） | — | — | spawn task-master next；失败回退本机 HTTP（:537） |
| GET | /api/taskmaster/tasks/:projectName | ui/server/routes/taskmaster.js:575 | 读（taskmaster.js:606） | — | — | 读 tasks.json |
| GET | /api/taskmaster/prd/:projectName | ui/server/routes/taskmaster.js:690 | 读（taskmaster.js:720/725） | — | — | 列 `.taskmaster/docs` |
| POST | /api/taskmaster/prd/:projectName | ui/server/routes/taskmaster.js:766 | 写（taskmaster.js:813） | — | — | mkdir:802 + writeFile |
| GET | /api/taskmaster/prd/:projectName/:fileName | ui/server/routes/taskmaster.js:851 | 读（taskmaster.js:880） | — | — | |
| DELETE | /api/taskmaster/prd/:projectName/:fileName | ui/server/routes/taskmaster.js:916 | 写（taskmaster.js:945） | — | — | unlink |
| POST | /api/taskmaster/init/:projectName | ui/server/routes/taskmaster.js:976 | 写（CLI:1004） | — | 是（:1025） | CLI 写 `.taskmaster` |
| POST | /api/taskmaster/add-task/:projectName | ui/server/routes/taskmaster.js:1066 | 写（CLI:1108） | — | 是（:1133） | CLI 写 tasks.json |
| PUT | /api/taskmaster/update-task/:projectName/:taskId | ui/server/routes/taskmaster.js:1171 | 写（CLI:1189/1242） | — | 是（:1210, :1263） | 两条 CLI 分支 |
| POST | /api/taskmaster/parse-prd/:projectName | ui/server/routes/taskmaster.js:1300 | 写（CLI:1342） | — | 是（:1363） | CLI 生成/追加 tasks.json |
| GET | /api/taskmaster/prd-templates | ui/server/routes/taskmaster.js:1402 | — | — | — | 内置模板常量 |
| POST | /api/taskmaster/apply-template/:projectName | ui/server/routes/taskmaster.js:1848 | 写（taskmaster.js:1903） | — | — | mkdir:1894 + writeFile |

## 3. `routes/config.js`（20）+ `routes/agent.js`（1）+ `routes/commands.js`（3）

挂载：`/api/config`（`index.js:620`，JWT）、`/api/agent`（`index.js:645`，**无 JWT**，仅 `validateExternalApiKey`，`agent.js:26`）、`/api/commands`（`index.js:607`，JWT）。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| GET | /api/config/connection-test-tasks | ui/server/routes/config.js:531 | 读（:500） | — | — | 内存任务；成功任务 `isCurrent` 读盘 |
| POST | /api/config/connection-test-tasks | ui/server/routes/config.js:540 | 读写（:542；persist :518） | 是（import:35 `src/model/catalog/index.js`，:547） | 是（:527） | 启动连接测试 |
| POST | /api/config/connection-test-tasks/:id/retry | ui/server/routes/config.js:555 | 读写（:518） | — | 是（:527） | |
| PUT | /api/config/connection-test-tasks/:id/image-capabilities | ui/server/routes/config.js:556 | 读写（:518） | — | 是（:527） | |
| POST | /api/config/connection-test-tasks/:id/acknowledge | ui/server/routes/config.js:557 | — | — | — | 仅内存 |
| POST | /api/config/connection-test-tasks/:id/cancel | ui/server/routes/config.js:558 | — | — | — | 中止内存任务 |
| GET | /api/config | ui/server/routes/config.js:643 | 读（:645） | — | — | 脱敏快照 |
| POST | /api/config/validate | ui/server/routes/config.js:652 | — | — | — | 仅校验 |
| GET | /api/config/model-references | ui/server/routes/config.js:663 | 读（:672） | — | — | |
| POST | /api/config/model-removal | ui/server/routes/config.js:698 | 读写（:715；:747 `writeRawPilotDeckYaml`） | — | 是（:756） | dryRun/原子删除 |
| GET | /api/config/office-preview/status | ui/server/routes/config.js:775 | 读（`officePreview.js:25, :237`） | — | — | LibreOffice 探测 |
| PUT | /api/config | ui/server/routes/config.js:803 | 读写（:823；:916/:979） | 是（:35；:293/:866） | 是（:995） | raw YAML / 结构化两路 |
| POST | /api/config/reload | ui/server/routes/config.js:1016 | 读（:1018） | — | 是（:1038） | 重载并广播 |
| GET | /api/config/provider | ui/server/routes/config.js:1045 | 读（:1050） | 是（:35，`PROVIDER_CATALOG` :1047） | — | 主 provider 视图 |
| POST | /api/config/models | ui/server/routes/config.js:1096 | 读（:1114） | 是（:34 `src/model/providerEndpoint.js` :1135；:35） | — | 拉远端模型（网络） |
| POST | /api/config/test-connection | ui/server/routes/config.js:1175 | 读（:1200） | 是（:37 `src/model/catalog/lookup.js` :1238；:35） | — | 探测连通性+图片能力 |
| POST | /api/config/test-connections | ui/server/routes/config.js:1289 | 读（:1274） | 是（:35，:152） | — | 复用 onboarding 多模型测试 |
| PUT | /api/config/test-connections/:testId/image-capabilities | ui/server/routes/config.js:1290 | — | — | — | 仅内存 |
| POST | /api/config/test-web-search | ui/server/routes/config.js:1298 | 读（:1318，条件） | 是（:1 `webSearchProviders.js`；:2 `additionalWebSearch.js`；:36 `src/network/fetch.js`） | — | 网络探测 |
| POST | /api/config/open | ui/server/routes/config.js:1510 | 读写（:1513-1517） | — | — | 缺配置时写默认 |
| POST | /api/agent | ui/server/routes/agent.js:828 | 读写（:344/:886 access；:365 mkdir；:379 clone；:428/:435 rm；sqlite import:7） | — | 是（SSE `writer.send` :918，非 WS） | 唯一入口；API-key 鉴权；可 clone/建分支/建 PR |
| SSE | /api/agent（stream=true） | ui/server/routes/agent.js:910 | 读写（同一处理链） | — | 是（:918） | 响应升级为 `text/event-stream` |
| SSE | /api/agent（异常路径） | ui/server/routes/agent.js:1184 | 读写（同一处理链） | — | 是（:1192） | 补设 SSE 头并回 error 帧 |
| POST | /api/commands/list | ui/server/routes/commands.js:887 | 读（legacy 回退 `fs.readFile` :85） | 是（import:15 `ChannelCommandRegistry.js`，:259） | — | 优先 gateway.commandsList |
| POST | /api/commands/load | ui/server/routes/commands.js:948 | 读（`fs.readFile` :970） | — | — | 读单个命令/SKILL 文件 |
| POST | /api/commands/execute | ui/server/routes/commands.js:1000 | 读写（:1103；`/skill_install` 经 clawhub 写盘 :780） | 是（import:16 `chatSearch.js` :292；:15） | — | 内建命令分派 |

## 4. `routes/projects.js`（11）+ `routes/modules.js`（13）+ `routes/staffdeck-copy.js`（1）+ `routes/skills.js`（11）+ `routes/memory.js`（27）

挂载：`/api/projects`（`index.js:589`）、`/api/modules`（`index.js:642`，工厂 `createModuleRuntimeRouter`）、`/api/modules/staffdeck-copy`（`modules.js:77`）、`/api/skills`（`index.js:612`）、`/api/memory`（`index.js:601`）。
`/api/modules` 每条请求先过 `modules.js:58`（条件绑定中间件，仅 `STAFFDECK_PROJECT_BINDING_ENABLED=true` 时调 `src/pilot/staffdeck/projectBinding.js`，import 在 `modules.js:16`）与 `modules.js:73`（abort 中间件）；`configFor`→`readConfig`（`modules.js:52-56`）读 `PILOTDECK_CONFIG_PATH` 或 `~/.pilotdeck/pilotdeck.yaml`。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| GET | /api/projects/:projectName/staffdeck-binding | ui/server/routes/projects.js:28 | 读写（:32；`projectBinding.ts:48/58`） | 是（:6） | — | 绑定状态刷新 |
| POST | /api/projects/:projectName/staffdeck-binding/ensure | ui/server/routes/projects.js:37 | 读写（:44；`projectBinding.ts:277-278`） | 是（:6） | — | 仅绑定用户，否则 403 |
| GET | /api/projects/:projectName/discovery-context | ui/server/routes/projects.js:302 | 读（`discovery-plans.js:74-80`） | 是（:14-21→`discovery-plans.js:24-31`） | — | 虚拟项目 403（:235） |
| GET | /api/projects/:projectName/discovery-plans | ui/server/routes/projects.js:303 | 读（`DiscoveryPlanService.ts:228`） | 是（:14→`discovery-plans.js:24`） | — | |
| POST | /api/projects/:projectName/discovery-plans/:planId/execute | ui/server/routes/projects.js:304 | 写（`discovery-plans.js:90`） | 是（:14） | — | gateway alwaysOnRerunPlan |
| GET | /api/projects/:projectName/discovery-plans/:planId/report | ui/server/routes/projects.js:306 | 读（`DiscoveryPlanService.ts:546/609`） | 是（:14） | — | |
| GET | /api/projects/:projectName/work-cycles | ui/server/routes/projects.js:322 | 读（`getCyclesOverview`） | 是（:14） | — | |
| POST | /api/projects/:projectName/work-cycles/:cycleId/apply | ui/server/routes/projects.js:336 | 写（`discovery-plans.js:116`） | 是（:14） | — | gateway alwaysOnApply |
| POST | /api/projects/:projectName/work-cycles/:cycleId/archive | ui/server/routes/projects.js:352 | 写（`DiscoveryPlanService.ts:366`） | 是（:14） | — | |
| POST | /api/projects/create-workspace | ui/server/routes/projects.js:379 | 读写（:433; :445/:477; :421/:486/:496） | 是（:10→`ui/server/projects.js:24`） | — | 建目录+可选 clone；token 走 sqlite（:518-522） |
| SSE | /api/projects/clone-progress | ui/server/routes/projects.js:539（头 :542） | 读写（:567; :573/:658; :589; :640） | 是（:10→`ui/server/projects.js:24`） | — | spawn git 进度转发，req close 杀进程 |
| USE | /api/projects（挂载说明） | ui/server/routes/projects.js:25（挂载 `index.js:589`） | — | — | — | router 定义行 |
| USE | /api/modules（绑定中间件） | ui/server/routes/modules.js:58 | 读写（:63/:66） | 是（:16） | — | 条件 |
| USE | /api/modules（abort 中间件） | ui/server/routes/modules.js:73 | — | — | — | |
| USE | /api/modules/staffdeck-copy | ui/server/routes/modules.js:77 | — | — | — | 子挂载 |
| POST | /api/modules/host-capabilities/call | ui/server/routes/modules.js:120 | — | 是（:16 中间件） | — | 代理；可流式 SSE/NDJSON（:100-112） |
| POST | /api/modules/host-capabilities/callback | ui/server/routes/modules.js:121 | — | 是（:16） | — | 同上 |
| GET | /api/modules/runtime | ui/server/routes/modules.js:128 | 读（:55 `readFileSync pilotdeck.yaml`） | 是（:16） | — | 脱敏模块运行时；gateway 能力探测 1s |
| POST | /api/modules/knowledge/query | ui/server/routes/modules.js:155 | —（代理） | 是（:16） | — | fetch 上游（:180） |
| POST | /api/modules/knowledge/citation | ui/server/routes/modules.js:198 | —（代理） | 是（:16） | — | fetch 上游（:214） |
| POST | /api/modules/knowledge/call | ui/server/routes/modules.js:232 | —（代理） | 是（:16） | — | 白名单；上游 JSON（:493） |
| GET | /api/modules/sop/definitions | ui/server/routes/modules.js:262 | 读（:547 `readFileSync`） | 是（:16） | — | |
| PUT | /api/modules/sop/definitions/:definitionId | ui/server/routes/modules.js:271 | 写（:284 `writeFileSync`） | 是（:16） | — | 写后 `restartRequired` |
| GET | /api/modules/sop/management | ui/server/routes/modules.js:291 | 读（`staffdeck-publish-route.js:107`） | 是（:16） | — | 校验 StaffDeck 身份后读 |
| POST | /api/modules/sop/management/call | ui/server/routes/modules.js:303 | 读写（`staffdeck-publish-route.js:69/94`, :84） | 是（:16） | — | 转发 owner API |
| POST | /api/modules/staffdeck-sdk/call | ui/server/routes/modules.js:449 | 写（`staffdeck-publish-route.js:94`；其余代理） | 是（:16） | — | 固定 SDK 白名单 |
| GET | /api/modules/staffdeck-sdk/events | ui/server/routes/modules.js:450 | —（代理） | 是（:16） | — | SSE 代理（:420-426） |
| POST | /api/modules/staffdeck-sdk/file | ui/server/routes/modules.js:453 | —（代理） | 是（:16） | — | multipart 转 base64（:463-468） |
| POST | /api/modules/staffdeck-copy/call | ui/server/routes/staffdeck-copy.js:132 | —（代理；import_resources 写远端） | 是（`modules.js:16` 中间件） | — | list_* 读远端；admin=0 禁写 |
| POST | /api/skills/list | ui/server/routes/skills.js:220 | 读（gateway skillsList） | —（gateway WS RPC） | — | `callGateway` :211 |
| POST | /api/skills/read | ui/server/routes/skills.js:247 | 读（gateway skillRead） | — | — | |
| POST | /api/skills/write | ui/server/routes/skills.js:263 | 写（gateway skillWrite） | — | — | 内容必须 string |
| POST | /api/skills/create | ui/server/routes/skills.js:283 | 写（gateway skillCreate） | — | — | |
| POST | /api/skills/delete | ui/server/routes/skills.js:303 | 写（gateway skillDelete） | — | — | |
| POST | /api/skills/validate | ui/server/routes/skills.js:319 | 读（gateway skillValidate） | — | — | |
| POST | /api/skills/import | ui/server/routes/skills.js:332 | 写（gateway skillImport） | — | — | |
| POST | /api/skills/scan | ui/server/routes/skills.js:351 | 读（gateway skillScan） | — | — | |
| POST | /api/skills/import-upload | ui/server/routes/skills.js:369 | 读写（:446-459） | — | — | multipart 落 `~/.pilotdeck/skills` 或项目内 |
| POST | /api/skills/clawhub/search | ui/server/routes/skills.js:503 | —（外部 CLI） | — | — | `execFile clawhub`，超时 30s |
| POST | /api/skills/clawhub/install | ui/server/routes/skills.js:553 | 写（:585, :602） | — | — | 安装到 skills 目录 |
| GET | /api/memory/overview | ui/server/routes/memory.js:337 | 读（`memoryService.js:50-51` control.sqlite/memory） | 是（:5 → `src/context/memory/edgeclaw-memory-core`） | — | 附 scheduler 状态 |
| GET | /api/memory/settings | ui/server/routes/memory.js:347 | 读（`pilotdeckConfig.js:771`） | 是（:8→`pilotdeckConfig.js:1,8-10`） | — | |
| POST | /api/memory/settings | ui/server/routes/memory.js:351 | 写（`pilotdeckConfig.js:956`） | 是（:8） | — | 写后 reload |
| POST | /api/memory/index/run | ui/server/routes/memory.js:356 | 读写 | 是（:5） | — | 手动 flush |
| POST | /api/memory/dream/run | ui/server/routes/memory.js:369 | 读写 | 是（:5） | — | |
| POST | /api/memory/dream/rollback-last | ui/server/routes/memory.js:382 | 读写 | 是（:5） | — | |
| GET | /api/memory/snapshot | ui/server/routes/memory.js:395 | 读（service.snapshot） | 是（:5） | — | |
| GET | /api/memory/memory/list | ui/server/routes/memory.js:401 | 读（service.list） | 是（:5） | — | |
| GET | /api/memory/memory/get | ui/server/routes/memory.js:417 | 读（service.get） | 是（:5） | — | |
| POST | /api/memory/memory/actions | ui/server/routes/memory.js:430 | 写（`service.ts:1099+`） | 是（:5） | — | edit/delete/deprecate/restore |
| GET | /api/memory/memory/user-summary | ui/server/routes/memory.js:442 | 读 | 是（:5） | — | |
| GET | /api/memory/project-meta | ui/server/routes/memory.js:448 | 读 | 是（:5） | — | general 走 repository |
| POST | /api/memory/project-meta | ui/server/routes/memory.js:461 | 写 | 是（:5） | — | |
| GET | /api/memory/workspace | ui/server/routes/memory.js:472 | 读（:203/:254 `readFileSync MEMORY.md`） | 是（:5） | — | |
| GET | /api/memory/cases | ui/server/routes/memory.js:485 | 读（`listCaseTraces`） | 是（:5） | — | |
| GET | /api/memory/cases/:caseId | ui/server/routes/memory.js:491 | 读 | 是（:5） | — | 未命中 404 |
| GET | /api/memory/index-traces | ui/server/routes/memory.js:501 | 读 | 是（:5） | — | |
| GET | /api/memory/index-traces/:indexTraceId | ui/server/routes/memory.js:507 | 读 | 是（:5） | — | 未命中 404 |
| GET | /api/memory/dream-traces | ui/server/routes/memory.js:517 | 读 | 是（:5） | — | |
| GET | /api/memory/dream-traces/:dreamTraceId | ui/server/routes/memory.js:523 | 读 | 是（:5） | — | 未命中 404 |
| GET | /api/memory/export/current-project | ui/server/routes/memory.js:533 | 读（`exportBundle`） | 是（:5） | — | Content-Disposition |
| GET | /api/memory/export/all-projects | ui/server/routes/memory.js:540 | 读（`memoryService.js:512`） | 是（:5） | — | |
| POST | /api/memory/import/current-project | ui/server/routes/memory.js:551 | 写（`importBundle`） | 是（:5） | — | 校验失败 400 |
| POST | /api/memory/import/all-projects | ui/server/routes/memory.js:564 | 写（`memoryService.js:536`） | 是（:5） | — | |
| GET | /api/memory/export | ui/server/routes/memory.js:575 | 读 | 是（:5） | — | |
| POST | /api/memory/import | ui/server/routes/memory.js:582 | 写 | 是（:5） | — | |
| POST | /api/memory/clear | ui/server/routes/memory.js:595 | 读写（`memoryService.js:496`） | 是（:5） | — | all_memory vs current_project |

> 任务书里「删 transcript 与项目目录（`projects.js:568-593`）」指的是 **helper 模块** `ui/server/projects.js`（`deleteSession` 545-579、`fs.unlink` :568、`fs.rm` :576；`deleteProject` 582-598、`fs.rm(projectDir)` :593），**不是** `ui/server/routes/projects.js`；其 HTTP 入口是 `index.js:954` 与 `:1004`（已列入第 1 节）。

## 5. `routes/gateway.js`（16）+ `mcp.js`（10）+ `mcp-utils.js`（2）+ `onboarding.js`（5）+ `settings.js`（15）+ `user.js`（6）+ `auth.js`（5）+ `update.js`（2；1 条未挂载）+ `webUpdate.js`（5）+ `models.js`（1）

挂载：`/api/gateway`（`index.js:627`）、`/api/mcp`（:595）、`/api/mcp-utils`（:604）、`/api/v1`（:624）、`/api/settings`（:617）、`/api/user`（:630）、`/api/auth`（:543）、`/api/update`（:648）、`/api/models`（:614）。`webUpdate.js` 不在 `index.js` 挂载，作为子路由挂在 `update.js:20` 下，故完整前缀 `/api/update/*`。这 10 个文件顶层均无直接 `src/` import。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| GET | /api/gateway/status | ui/server/routes/gateway.js:147 | 读（:149; :56; :66） | — | — | 飞书/微信/企微汇总 |
| POST | /api/gateway/feishu/test | ui/server/routes/gateway.js:191 | — | — | — | 网络校验 |
| POST | /api/gateway/feishu/qr-begin | ui/server/routes/gateway.js:241 | — | — | — | 存 `app.locals` |
| GET | /api/gateway/feishu/qr-poll | ui/server/routes/gateway.js:284 | 读写（:316） | — | 是（:124） | 成功写配置并广播 |
| POST | /api/gateway/feishu/qr-cancel | ui/server/routes/gateway.js:358 | — | — | — | |
| POST | /api/gateway/feishu/save | ui/server/routes/gateway.js:363 | 读写（:370） | — | 是（:124） | |
| POST | /api/gateway/feishu/disable | ui/server/routes/gateway.js:389 | 读写（:391） | — | 是（:124） | |
| POST | /api/gateway/weixin/qr-begin | ui/server/routes/gateway.js:405 | 读写（:408） | — | 是（:124） | |
| GET | /api/gateway/weixin/qr | ui/server/routes/gateway.js:443 | 读（:445） | — | — | |
| GET | /api/gateway/weixin/qr-poll | ui/server/routes/gateway.js:459 | 读（:460-461） | — | — | |
| POST | /api/gateway/weixin/disable | ui/server/routes/gateway.js:478 | 读写（:480） | — | 是（:124） | |
| POST | /api/gateway/wecom/qr-begin | ui/server/routes/gateway.js:494 | — | — | — | |
| GET | /api/gateway/wecom/qr-poll | ui/server/routes/gateway.js:521 | 读写（:548） | — | 是（:124） | |
| POST | /api/gateway/wecom/qr-cancel | ui/server/routes/gateway.js:571 | — | — | — | |
| POST | /api/gateway/wecom/save | ui/server/routes/gateway.js:576 | 读写（:590） | — | 是（:124） | |
| POST | /api/gateway/wecom/disable | ui/server/routes/gateway.js:616 | 读写（:618） | — | 是（:124） | |
| GET | /api/mcp/config | ui/server/routes/mcp.js:26 | 读（:29 `listMcpConfigFiles`） | — | — | |
| GET | /api/mcp/config/:scope | ui/server/routes/mcp.js:36 | 读（:43） | — | — | global/project |
| POST | /api/mcp/config/validate | ui/server/routes/mcp.js:50 | — | — | — | 仅内存 |
| PUT | /api/mcp/config/:scope | ui/server/routes/mcp.js:60 | 写（:68 `writeMcpConfigFile`） | — | — | 保存并触发 gateway reload |
| GET | /api/mcp/cli/list | ui/server/routes/mcp.js:89 | 读（:93 spawn claude CLI） | — | — | |
| POST | /api/mcp/cli/add | ui/server/routes/mcp.js:129 | 写（:176 经 CLI） | — | — | |
| POST | /api/mcp/cli/add-json | ui/server/routes/mcp.js:211 | 写（:268 经 CLI） | — | — | |
| DELETE | /api/mcp/cli/remove/:name | ui/server/routes/mcp.js:302 | 写（:335 经 CLI） | — | — | |
| GET | /api/mcp/cli/get/:name | ui/server/routes/mcp.js:371 | 读（:377 经 CLI） | — | — | |
| GET | /api/mcp/config/read | ui/server/routes/mcp.js:413 | 读（:429 `fs.readFile ~/.claude.json`） | — | — | **被 :36 遮蔽，不可达** |
| GET | /api/mcp-utils/taskmaster-server | ui/server/routes/mcp-utils.js:18 | 读（`mcp-detector.js:31`） | — | — | `~/.pilotdeck*.json` |
| GET | /api/mcp-utils/all-servers | ui/server/routes/mcp-utils.js:35 | 读（`mcp-detector.js:164`） | — | — | |
| GET | /api/v1/providers | ui/server/routes/onboarding.js:261 | — | — | — | 内置 provider 目录 |
| POST | /api/v1/model-connection-tests | ui/server/routes/onboarding.js:409 | — | — | — | 结果存内存 |
| PUT | /api/v1/model-connection-tests/:testId/image-capabilities | ui/server/routes/onboarding.js:411 | — | — | — | 内存 |
| PUT | /api/v1/model-configuration | ui/server/routes/onboarding.js:413 | 读写（:433; :480 写 yaml） | — | — | |
| POST | /api/v1/workspaces | ui/server/routes/onboarding.js:499 | 读写（:522-563; :535; :561） | — | — | 建/克隆工作区 |
| GET | /api/settings/permissions | ui/server/routes/settings.js:16 | 读（:18; `permissionSettings.js:57`） | — | — | |
| PUT | /api/settings/permissions | ui/server/routes/settings.js:25 | 写（:27; `permissionSettings.js:74`） | — | — | |
| GET | /api/settings/api-keys | ui/server/routes/settings.js:40 | 读（:42 sqlite） | — | — | 脱敏 |
| POST | /api/settings/api-keys | ui/server/routes/settings.js:56 | 写（:64） | — | — | |
| DELETE | /api/settings/api-keys/:keyId | ui/server/routes/settings.js:76 | 写（:79） | — | — | |
| PATCH | /api/settings/api-keys/:keyId/toggle | ui/server/routes/settings.js:93 | 写（:102） | — | — | |
| GET | /api/settings/credentials | ui/server/routes/settings.js:120 | 读（:123） | — | — | |
| POST | /api/settings/credentials | ui/server/routes/settings.js:133 | 写（:149） | — | — | |
| DELETE | /api/settings/credentials/:credentialId | ui/server/routes/settings.js:168 | 写（:171） | — | — | |
| PATCH | /api/settings/credentials/:credentialId/toggle | ui/server/routes/settings.js:185 | 写（:194） | — | — | |
| GET | /api/settings/notification-preferences | ui/server/routes/settings.js:211 | 读（:213） | — | — | |
| PUT | /api/settings/notification-preferences | ui/server/routes/settings.js:221 | 写（:223） | — | — | |
| GET | /api/settings/push/vapid-public-key | ui/server/routes/settings.js:235 | 读写（:237; `vapid-keys.js:7-14`） | — | — | |
| POST | /api/settings/push/subscribe | ui/server/routes/settings.js:245 | 写（:251/:256） | — | — | |
| POST | /api/settings/push/unsubscribe | ui/server/routes/settings.js:279 | 写（:285/:290） | — | — | |
| GET | /api/user/git-config | ui/server/routes/user.js:33 | 读写（:36; :44） | — | — | 无库则回填系统 git |
| POST | /api/user/git-config | ui/server/routes/user.js:62 | 写（:77; :80-81） | — | — | 写 DB + 全局 git |
| POST | /api/user/complete-onboarding | ui/server/routes/user.js:98 | 读（:100） | — | — | 校验模型配置就绪 |
| GET | /api/user/onboarding-status | ui/server/routes/user.js:119 | 读（:121） | — | — | |
| GET | /api/user/runtime-status | ui/server/routes/user.js:135 | 读（:137） | — | — | |
| POST | /api/user/runtime/gateway/retry | ui/server/routes/user.js:150 | 读（:152） | — | — | 请求 gateway 重启（IPC） |
| GET | /api/auth/status | ui/server/routes/auth.js:17 | 读（:27 `userDb.hasUsers`） | — | — | |
| POST | /api/auth/register | ui/server/routes/auth.js:40 | 写（:57 BEGIN/:76 COMMIT; :71 createUser） | — | — | 单用户，仅首次 |
| POST | /api/auth/login | ui/server/routes/auth.js:102 | 读写（:115; :130 last login） | — | — | 发 JWT |
| GET | /api/auth/user | ui/server/routes/auth.js:145 | — | — | — | |
| POST | /api/auth/logout | ui/server/routes/auth.js:152 | — | — | — | 无状态 |
| USE | /api/update（子挂载） | ui/server/routes/update.js:20 | — | — | — | 挂 webUpdate |
| POST | /api/update/restart | ui/server/routes/update.js:108 | 读写（`updateRuntime.js:152`） | — | — | docker/supervisor/direct 三模式 |
| POST | /api/update/restart（工厂） | ui/server/routes/update.js:112 | 读写 | — | — | `createUpdateRouter` 内，**index.js 未挂载**，仅测试用 |
| GET | /api/models | ui/server/routes/models.js:7 | 读（:9 `getModelConfigurationState`） | — | — | 经 gateway |
| GET | /api/update/context | ui/server/routes/webUpdate.js:8 | 读（:8 `realpathSync`） | — | — | 子挂载 |
| GET | /api/update/info | ui/server/routes/webUpdate.js:9 | — | — | — | 启动时冻结 |
| POST | /api/update/check | ui/server/routes/webUpdate.js:10 | 读写（`webUpdateService.js:96`; git fetch） | — | — | 查 GitHub release |
| GET | /api/update/status | ui/server/routes/webUpdate.js:11 | — | — | — | 内存状态 |
| POST | /api/update/apply | ui/server/routes/webUpdate.js:12 | 写（`webUpdateService.js:121, :137, :157, :168-173`） | — | — | NDJSON 流式 |

## 6. `routes/messages.js`（3）+ `plugins.js`（8）+ `sop.js`（4）+ `uploads.js`（7）

挂载：`/api/sessions`（`index.js:640`）、`/api/plugins`（:633）、`/api/sop`（:641）、`/api/uploads`（:613），全部带 `authenticateToken`。

| 方法 | 完整路径 | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|---|
| GET | /api/sessions/:sessionId/messages | ui/server/routes/messages.js:42 | 读（:52 gateway.readSessionMessages → `src/web/server/readSessionMessages.ts`） | —（经 :19 bridge） | — | limit/offset/relativeTranscriptPath；无 gateway 503 |
| POST | /api/sessions/:sessionId/fork | ui/server/routes/messages.js:100 | 写（:110 → `src/web/server/forkSession.ts:500, :10`） | —（经 :19 bridge） | — | `fromEntryId` 必填 |
| GET | /api/sessions/:sessionId/subagent/:subagentId/messages | ui/server/routes/messages.js:137 | 读（:142 → `readSubagentWebMessages`） | —（经 :19 bridge） | — | 失败返空 messages |
| GET | /api/plugins | ui/server/routes/plugins.js:27 | 读（:29 `scanPlugins` → `plugin-loader.js:148-171, :150`） | — | — | 列插件 + serverRunning |
| GET | /api/plugins/:name/manifest | ui/server/routes/plugins.js:40 | 读（:45） | — | — | 名字 `^[a-zA-Z0-9_-]+$` |
| GET | /api/plugins/:name/assets/* | ui/server/routes/plugins.js:57 | 读（:74 statSync; :88 createReadStream） | — | — | no-store；防穿越 |
| PUT | /api/plugins/:name/enable | ui/server/routes/plugins.js:100 | 读写（:113; :115 `savePluginsConfig`） | — | — | 按需启停子进程 |
| POST | /api/plugins/install | ui/server/routes/plugins.js:140 | 写（:152 → `plugin-loader.js:253`） | — | — | 仅 https:// 或 git@ |
| POST | /api/plugins/:name/update | ui/server/routes/plugins.js:173 | 写（:186 → `plugin-loader.js:376`） | — | — | 先停后更 |
| ALL | /api/plugins/:name/rpc/* | ui/server/routes/plugins.js:207 | —（本机 http 代理到插件端口） | — | — | 注入 `x-plugin-secret-*` |
| DELETE | /api/plugins/:name | ui/server/routes/plugins.js:286 | 写（:300 → `plugin-loader.js:439`） | — | — | 先停再删 |
| POST | /api/sop/approver/login | ui/server/routes/sop.js:90 | —（外发 fetch） | —（经 :4 bridge） | — | StaffDeck 登录 |
| GET | /api/sop/approver/session | ui/server/routes/sop.js:91 | — | — | — | StaffDeck `/api/auth/me` |
| GET | /api/sop/status | ui/server/routes/sop.js:93 | — | —（gateway.sopStatus） | — | 无能力 501 |
| POST | /api/sop/resume | ui/server/routes/sop.js:117 | — | —（gateway.resumeSop） | — | `source=human` 需绑定用户 |
| GET | /api/uploads/limits | ui/server/routes/uploads.js:102 | —（只读 env） | 是（:8 `src/gateway/dialog/UploadStore.js`；:11 `uploadLimits.js:1`） | — | |
| POST | /api/uploads | ui/server/routes/uploads.js:104 | 写（:106 → `UploadStore.ts:92-93`） | 是（:8） | — | 201；支持 Idempotency-Key |
| POST | /api/uploads/:uploadId/content | ui/server/routes/uploads.js:121 | 读写（:94 → `UploadStore.ts:125, :162`） | 是（:8） | — | 字段名 `files[<clientFileId>]` |
| SSE | /api/uploads/:uploadId/events | ui/server/routes/uploads.js:137（头 :152） | 读（:150; :142） | 是（:8） | — | 推 `upload_*` |
| GET | /api/uploads/:uploadId/attachments/:attachmentId/preview | ui/server/routes/uploads.js:169 | 读（:182 → `UploadStore.ts:464`） | 是（:8） | — | 仅 image/*；base64 data URL |
| GET | /api/uploads/:uploadId | ui/server/routes/uploads.js:189 | 读（:190 → `UploadStore.ts:384`） | 是（:8） | — | |
| DELETE | /api/uploads/:uploadId | ui/server/routes/uploads.js:194 | 写（:195 → `UploadStore.ts:210`） | 是（:8） | — | 204 |

`routes/cron-jobs.js`：94 行、无 `express`/`router`，只导出 `createCronUpdateHandler`（:1），被 `index.js:114` import、`index.js:752` 注册；**零路由注册**。

---

## 7. WebSocket 与入站消息 type（26 种）

| 端点 | 定义位置 | 说明 |
|---|---|---|
| `wss`（升级 `/ws`、`/shell`、`/plugin-ws/*`） | ui/server/index.js:472-507 | `verifyClient` 鉴权（`?token=` 或 `Authorization`，:491-496）；`:2291-2308` 按 pathname 分发 |
| WS `/ws` | ui/server/index.js:2301 | `handleChatConnection` :2358；`connectedClients` :2364；断线清理 :2645-2651 |
| WS `/shell` | ui/server/index.js:2299 | `handleShellConnection` :2664；`node-pty` 导入 :58；`terminalSessions` 复用 |
| WS `/plugin-ws/*` | ui/server/index.js:2303 | `handlePluginWsProxy` :2250-2288；纯代理到插件 `127.0.0.1:<port>/ws` |

| 消息 type | 定义位置 | 读写磁盘 | 调 src/ | WS 广播依赖 | 备注 |
|---|---|---|---|---|---|
| `WS /ws :: ping` | ui/server/index.js:2379 | — | — | — | no-op |
| `WS /ws :: get-session-activity` | ui/server/index.js:2380 | — | — | 是（:2381） | activity snapshot |
| `WS /ws :: watch-session` | ui/server/index.js:2386 | — | 是（:2389） | 是（:2388, :2390） | |
| `WS /ws :: unwatch-session` | ui/server/index.js:2395 | — | — | 是（:2397） | |
| `WS /ws :: pilotdeck-command` | ui/server/index.js:2403 | — | 是（:2436） | 是（:2372, :2431-2432） | 主聊天入口 |
| `WS /ws :: claude-command` | ui/server/index.js:2405 | — | 是（:2436） | 是 | 旧 provider 兼容 |
| `WS /ws :: cursor-command` | ui/server/index.js:2406 | — | 是（:2436） | 是 | 旧 provider 兼容 |
| `WS /ws :: codex-command` | ui/server/index.js:2407 | — | 是（:2436） | 是 | 旧 provider 兼容 |
| `WS /ws :: gemini-command` | ui/server/index.js:2408 | — | 是（:2436） | 是 | 旧 provider 兼容 |
| `WS /ws :: get-input-queue` | ui/server/index.js:2437 | — | 是（:2438） | 是（:2439） | |
| `WS /ws :: queue-input` | ui/server/index.js:2440 | — | 是（:2441） | 是（:2447） | |
| `WS /ws :: delete-queued-input` | ui/server/index.js:2454 | — | 是（:2455） | 是（:2462-2466, :2468） | 推给同用户全部 socket |
| `WS /ws :: move-queued-input` | ui/server/index.js:2475 | — | 是（:2476） | 是（:2477） | |
| `WS /ws :: steer-queued-input` | ui/server/index.js:2484 | — | 是（:2485） | 是（:2491） | |
| `WS /ws :: resume-input-queue` | ui/server/index.js:2498 | — | 是（:2499） | 是（:2504） | |
| `WS /ws :: regenerate-last-message` | ui/server/index.js:2511 | — | 是（:2529-2531） | 是（:2519, :2527） | 事务式重生成 |
| `WS /ws :: abort-session` | ui/server/index.js:2533 | — | 是（:2536-2537） | 是（:2538） | |
| `WS /ws :: permission-response` | ui/server/index.js:2539 | — | 是（:2541） | 是（:2551） | |
| `WS /ws :: session-permission-grant` | ui/server/index.js:2563 | — | 是（:2564） | 是（:2565） | 仅回请求方 |
| `WS /ws :: elicitation-response` | ui/server/index.js:2573 | — | 是（:2575） | 是（:2578） | |
| `WS /ws :: check-session-status` | ui/server/index.js:2590 | — | 是（:2596） | 是（:2593, :2602） | |
| `WS /ws :: get-pending-permissions` | ui/server/index.js:2617 | — | — | 是（:2621） | 空 stub |
| `WS /ws :: get-active-sessions` | ui/server/index.js:2626 | — | 是（:2627） | 是（:2631） | 保留四 provider 键 |
| `WS /shell :: init` | ui/server/index.js:2683 | 读（:2771 `fs.statSync`） | — | 是（:2726, :2762, :2922, :2950） | 起/复用 PTY |
| `WS /shell :: input` | ui/server/index.js:2981 | — | — | 是（:2985；onData 回发） | |
| `WS /shell :: resize` | ui/server/index.js:2994 | — | — | 是（:2998） | |

## 8. SSE / 流式端点

| 方法 | 路径 | 定义位置 | 形态 | 证据 |
|---|---|---|---|---|
| GET | `/api/search/conversations` | ui/server/index.js:1033 | `text/event-stream`，事件 `result/progress/done/error` | 头 :1043；`X-Accel-Buffering: no` :1046 |
| SSE | `/api/projects/clone-progress` | ui/server/routes/projects.js:539 | `data:` JSON | 头 :542；`flushHeaders` :545 |
| SSE | `/api/uploads/:uploadId/events` | ui/server/routes/uploads.js:137 | `data:` JSON，事件 `upload_*` | 头 :152 |
| POST→SSE | `/api/agent` | ui/server/routes/agent.js:828（SSE :910；异常 :1184） | `stream=true` 时升级 | `SSEStreamWriter` :449 |
| SSE 代理 | `/api/modules/host-capabilities/call`、`/api/modules/staffdeck-sdk/events` | ui/server/routes/modules.js:120, :450 | 透传上游 SSE/NDJSON | :100-112, :420-426 |
| NDJSON | `/api/update/apply` | ui/server/routes/webUpdate.js:12 | 流式 NDJSON | `webUpdateService.js:121+` |

---

## 9. 分母的构成：工厂 / 条件 / 循环 / 未挂载 / 遮蔽

> 264 是静态清点的注册数；下面这些让静态计数**不可复算**，运行时枚举（§10）必须单独识别并标注。

| 类 | 条目 | 位置 | 对分母的影响 |
|---|---|---|---|
| 工厂路由 | `createSessionModelHandlers()` → `GET/PUT/DELETE /api/sessions/model` | `routes/models.js:22`；调用 `index.js:636`；注册 `index.js:637-639` | 每实例 3 条；实例化前静态不可见 |
| 工厂路由 | `createModuleRuntimeRouter(...)` → `/api/modules/*`（本清单 13 条 + 2 中间件 + 1 子挂载） | `routes/modules.js:51`；挂载 `index.js:642` | 每实例由工厂产出；随挂载前缀变 |
| 工厂路由 | `createWorkspaceUploadCheckHandler` / `createWorkspaceFileUploadHandler` → `POST /api/projects/:projectName/files/upload/check`、`.../upload` | `index.js:2239, 2242, 2244` | 2 条，handler 由工厂产出 |
| 工厂路由（**未挂载**） | `createUpdateRouter()` 内的 `POST /api/update/restart` | `routes/update.js:110-112` | `index.js` 未挂载，仅测试引用；不计入可用分母 |
| 条件挂载 | `/api/modules` 绑定中间件，仅 `STAFFDECK_PROJECT_BINDING_ENABLED=true` 时调 `src/pilot/staffdeck/projectBinding.js` | `routes/modules.js:58-71` | 不增减路由数，但改变可达性/副作用；枚举标 `conditional` |
| 循环挂载 | 4 个 410 前缀 `/api/cursor`、`/api/codex`、`/api/gemini`、`/api/cli` | `index.js:669-675` | 4 条 `.use`（不计入 264），枚举展开成 4 条前缀 |
| 遮蔽（不可达） | `GET /api/mcp/config/read` 被 `GET /api/mcp/config/:scope` 按注册顺序遮蔽 | `mcp.js:413` vs `mcp.js:36` | 静态清单有、运行时可达性为零；枚举标 `shadowed` |
| 双注册 / 穿透 | `/api/projects/files`（`index.js:547`）先于 `/api/projects`（`:589`）；`/api/sessions/model`（`:637-639`）先于 `/api/sessions`（`:640`） | 上述行号 | 归属表按注册顺序判；各配 1 条探针用例 |

## 10. 运行时枚举（分母以枚举为准）与静态对账

静态清点已出现过漏项（`routes/memory.js:344, 448` 的 `router.route('/x').get().post()` 链式；`routes/update.js:112` 的 `restartRouter.post` 由方法名正则漏掉）；工厂 / 条件 / 循环使计数随 env 与实例化变。**分母以运行时枚举为准。**

**枚举做法**（装置 `scripts/ui-server-parity/enumerate_routes.mjs`）：
1. 隔离 env 下加载真 `ui/server`（高位端口 + 临时 `PILOT_HOME`/`DATABASE_PATH`，见计划 003 B1 S2）；
2. 加载期 hook：包 `express.Router.prototype.use` / `.route` 与各 METHOD（以及 `app.use` / `app.METHOD`），按调用顺序记录 `(order, method, mountPath, stackRef)`；
3. dump Express router stack 交叉校验：`app._router.stack` 递归，`route` 层取 `layer.route.path` + `methods()`，`router` 层取 `layer.handle.stack`，挂载前缀由 `layer.regexp` 还原；与 hook 记录对齐；
4. 输出 JSON：`[{order, method, fullPath, definition, kind: route|middleware, factory, conditional, loopPrefix, shadowed, mounted}]`；
5. 与本文静态清单按 `(method, fullPath, definition)` 对账，报告 `only-static` / `only-runtime` / `path-mismatch`；
6. **以枚举为准**回填本文；差异逐条给原因（链式漏计、工厂实例化、条件分支、未挂载、遮蔽）。

**对账产物**：`docs/experiments/exp008-ui-server-proxy/router-enumeration.json`（B1 首跑），并在 B2 起每次接管后重跑。

**B1 首跑结果（2026-10-14，exp008）**：运行时 dump `app._router.stack` 得 **263 条唯一 (method, path)**，与本节静态 264 条注册的差正好是 1——`routes/update.js:112` 的 `createUpdateRouter` 工厂路由**未挂载**，且与已挂载的 `update.js:108` 路径重复，去重后只剩 1 条。逐条对账的 `only-static` 仅剩静态清单里两条把 `/api/agent` 标成 `SSE` 的行（真实注册是 `POST`，属方法标签差异），`only-runtime` 为 0；详见 `docs/experiments/exp008-ui-server-proxy/route-reconciliation.json`。挂载前缀由 `layer.regexp` 还原（Express 4.22 的 Router 实例原型与 `express.Router.prototype` 不同，加载期 hook 拦不到，见 exp008 readme「意外」）。

## 11. 共享资源分组（迁移组）

> 「单条路由单写者」不成立：`routes/memory.js:79` 与 `/api/config` 同写 `pilotdeck.yaml`；`auth.db` 被多组路由共享。按共享资源划组，**同组同批**或由跨进程契约中介。完整决定与依据见 `decision.md` #7。

| 组 | 共享资源 | 成员（本节表） |
|---|---|---|
| G-YAML | `pilotdeck.yaml` | §3 `/api/config` 写路由（`config.js:518` 的 `POST connection-test-tasks`、`PUT /api/config`、`POST /api/config/reload`、`POST /api/config/open`）；§4 `POST /api/memory/settings`（`memory.js:351`）；§5 `/api/gateway/{feishu,weixin,wecom}/*` 保存类；§5 `PUT /api/v1/model-configuration`（`onboarding.js:413`） |
| G-AUTHDB | `auth.db` | §1 `DELETE /api/projects/:projectName/sessions/:sessionId`（`:963` `sessionNamesDb`）、`PUT /api/sessions/:sessionId/rename`（`:995`）；§5 `/api/auth/*`、`/api/settings/{api-keys,credentials,notification-preferences,push}/*`、`/api/user/git-config` |
| G-EVT | `pilotdeck:config-broadcast` + `connectedClients` | §1 `DELETE /api/projects/.../sessions/...`（`:966` `session-deleted`）；§2 taskmaster 写路由（`:1025, 1133, 1210, 1263, 1363`）；§3 `/api/config/*` 与 §5 gateway 保存 |
| G-PLUGINS | `plugins.json` + 插件目录 | §6 `/api/plugins/*` |
| G-SKILLS | skills 目录 | §4 `/api/skills/*` 写路由；§3 `/api/commands/execute` 的 `/skill_install` |
| G-TASKDIR | 项目 `.taskmaster/` | §2 taskmaster 写路由 |
| G-GIT | 项目 `.git` | §2 git 写路由 |
| G-MEMDB | 记忆 `control.sqlite`（≠ `auth.db`） | §4 `/api/memory/*`（memoryService） |
| G-PERMS | `<PILOT_HOME>/permissions.json` | §5 `GET/PUT /api/settings/permissions`；`index.js:653` 读 |
| G-UPLOAD | 上传目录 + `UploadStore` | §6 `/api/uploads/*` |
| G-SESS | transcript `projects/<id>/chats/*.jsonl` | §1 `DELETE /api/projects/:projectName/sessions/:sessionId`、`DELETE /api/projects/:projectName`；§6 `POST /api/sessions/:sessionId/fork` |

**归属表生成规则**：按注册顺序 + 挂载穿透（§9 的遮蔽 / 双注册各配探针）；每条路由标 `rust|ts` 与所属组。

---

## 口径与已知遮蔽

1. **计数口径**：264 = 本清单逐条清点的路由注册（非测试文件）；方法名正则脚本会漏掉 `router.route('/x').get().post()` 链式（`routes/memory.js:344, 448`）与 `restartRouter.post`（`routes/update.js:112`）。**本清单是静态输入，最终分母以 §10 的运行时枚举为准**；二者对账的差集逐条给原因。其中 `routes/update.js:112` 未挂载、**可用 263**（以枚举复核）。`.use` 挂载（36 处）不计入 264，但在本清单中以 `USE` 行列示。任务书的「约 280」应含 `.use`。
2. **`调 src/` 口径**：只算该 route 文件顶层**直接** import `src/`。`routes/messages.js`、`routes/sop.js`、`routes/skills.js` 等经 `pilotdeck-bridge.js`（`ui/server/pilotdeck-bridge.js:60` → `src/gateway/index.js`）到 `src/`，本表标 `—（经 bridge）` 并在备注注明；`routes/uploads.js`、`routes/projects.js`、`routes/modules.js`、`routes/commands.js`、`routes/config.js`、`routes/memory.js` 有直接 import。
3. **WS 广播口径**：只算 UI 端 `connectedClients` / `broadcast*` / `process.emit('pilotdeck:config-broadcast')`。与插件 gateway 的 WS RPC（`skills.js`、`modules.js`）不算广播。
4. **已发现的遮蔽/不可达**（完整构成见 §9）：`GET /api/mcp/config/read`（`mcp.js:413`）被 `GET /api/mcp/config/:scope`（`mcp.js:36`）按注册顺序遮蔽；`POST /api/update/restart`（`update.js:112`）只被测试引用、未挂载；`GET /api/projects` 在 `index.js:905` 与 `index.js:589` 挂载重叠（优先级以注册顺序为准）；`/api/sessions/model`（`:637-639`）先于 `/api/sessions`（`:640`）。
5. **`projects.js` 归属纠错**：删 transcript / 项目目录的 helper 在 `ui/server/projects.js:545-598`，HTTP 入口在 `index.js:954` 与 `:1004`；`ui/server/routes/projects.js` 内没有任何 DELETE 路由。
6. **未展开**：`ui/dist`、`ui/public`、`memory-dashboard` 静态资源逐个文件不列；`node-pty` 的 PTY 输出帧不列（`/shell` 的 `output`/`error` 出站帧无固定 type 清单，是 PTY 字节回流）。
