# 取舍卡 · 阶段 B：用 Rust 替换 `ui/server`

> 2026-10-14。**一页纸。** 依据见 `must-know.md` 与 `gap.md`（行号均已回源核对）；上一轮全局拍板见 `docs/research/002_parity-boundary/decision.md`（先 C 再 A 再 B、驱动用 TS、默认修 bug）。
> 授权依据：`CLAUDE.md` §五「授权扩大（2026-10-14 夜，人原话）……方向、红线、架构选型、阶段切换都可由 AI 代拍；每一处都记进晨报『AI 代批，待人复核』」。本卡每一处决策标 **AI 代拍：…（待人复核）**。

## 这次不可妥协的几条

1. 浏览器 `ui/src` 一行不改，Rust 对浏览器完全等价于原 `ui/server`（HTTP / 无类型 WS / SSE / 静态 / 鉴权语义）。
2. 能逐路由从 TS 切到 Rust、任何时刻可回退；未接管的 HTTP/SSE 与**整条** WS 连接原样反代给 TS `ui/server`。
3. 「一致」能由机器判、同一输入可复现：HTTP / WS / SSE / 落盘四类观测点可录可比，且有阳性对照。

## 我选择什么（AI 代拍）

> **甲：Rust 反代站上浏览器唯一入口（默认 3001；实验一律高位端口），逐路由接管，但按共享资源划迁移组；未接管的 HTTP / SSE 与整条 `/ws`、`/shell`、`/plugin-ws/*` 连接反向代理给跑在私有回环端口的 TS `ui/server`。不选整体替换。** 进程内 38 处 `src/` 依赖按「Rust 直接实现 / 改走阶段 A 的 gateway 协议 / 随宿主路由留守 TS」三桶处置（逐条见下 #2）。**接管粒度不是单条路由，而是共享资源组**（同一文件 / 同一 SQLite / 同一进程内事件，见 #7）——因为 `pilotdeck.yaml`、`auth.db`、`pilotdeck:config-broadcast` 都横跨多条路由。分母以**运行时枚举**（dump Express router stack）为准并与静态清单对账（见 #3）。对拍装置另立 `scripts/ui-server-parity/` + `specs/ui-server-parity/`（**不动**已冻结的 `specs/parity/`），用录制反代 + Playwright 场景 + 本地 mock 模型 + 阳性对照。

## 我放弃什么（AI 代拍）

> 放弃「一上来整体重写 264 条路由 + 3 个 WS 端点」；放弃「按 WS message type 拆给两个后端」（连接级内存态，`index.js:2364, 2388, 2337-2341`）；放弃改 `ui/src` 去适配 Rust；放弃「一条路由只有一个写者」这种按路由的写法——`routes/memory.js:79` 与 `/api/config` 写同一份 `pilotdeck.yaml`、`auth.db` 被 `routes/auth.js` / `routes/settings.js:2` / `routes/user.js:2` / `index.js:963,995` 多组共享，改为**按共享资源分组、同组同批**（#7）；放弃用 `ui/server/*.test.*` 的单路由内存 express 测试当后端对拍；放弃动 `specs/parity/` 冻结文件。

## 依据来自哪一类

- [x] 自有积累 —— 调研 002（边界选型与三条命门）、exp002（连接级状态，可类比 3001 的 WS）、`docs/plan/001`（阶段 C 的装置方法论：装置先于代码、阳性对照、冻结规约）、阶段 A 的 `docs/research/005_phase-a-gateway/`（同为「Rust 前置 + 未实现透传」的姊妹方案）
- [x] 他人积累 —— PilotDeck 基线 `cc7c4e6`（本轮逐条回源核对，见 `gap.md` 与 `route-inventory.md`）；prime-agent（mock 模型 HTTP 服务、归一化、CI 对比 sha 固定旧版，`findings-prime-agent.md:20, 39, 43`）
- [ ] 自己跑的 —— 探针（计划 003 的 Issue B1）还没跑，是本阶段第一条 issue

## 支持它的最强依据

入口在基线里**本来就是可重定向的、且已有一条纯反代样板**：`SERVER_PORT` / `HOST`（`index.js:3583-3584`）、`DATABASE_PATH`（`db.js:26`）、`PILOT_HOME`（`index.js:190`）全部读环境变量；`ui/server` 可脱离 supervisor 单独起（`runtimeCoordination.js:12-14` 非监督即 `unmanaged`）；`handlePluginWsProxy` 已示范「双向逐帧搬运 + 双向传播 close」（`index.js:2250-2288`）。`ui/e2e/real/launch.mjs` 已经用高位端口 + 临时 `PILOT_HOME` / `DATABASE_PATH` 编排整套进程（`launch.mjs:11-14, 78, 83-84`），可直接当探针骨架。

## 反对它的最强依据

反代态的一致是**必要不充分**：它证明入口搬运与装置，但逐条接管时每一步都引入新的不一致面——Express 路由优先级 / 中间件顺序（`/api/projects/files` 必须先于 `/api/projects`，`index.js:545-547, 589`）、3001 的无类型 WS 有连接级内存态（`index.js:2364, 2388, 2337-2341`）、以及 38 处进程内 `src/` 依赖中无法改走 gateway 协议的那些必须由 Rust 直接重实现。且阶段 A / C / B 并行共享机器与「全量对拍同一时刻只跑 1 个」的约束（`CLAUDE.md` §五），并行会互相制造假差异。

## 仍未补齐的缺口

① 探针没跑——「真 `ui/server` 在私有端口、Rust 反代在前、真浏览器一切照常」是全部方案的前提实证（计划 003 Issue B1）。② 阶段 A 的 gateway method 清单还没冻结，`src/web/server/*` 等能否改走 gateway 协议要等它（下 #2 里已标「待阶段 A 确认」）。③ 3001 的 `/ws` 帧序列没有金样本，无类型消息的比对口径只能从实测里长出来（gap.md 3.2）。④ 跨进程广播桥（#7）与文件 watcher 的二次广播语义没实测，桥的 payload 形状、鉴权、失败重试都要探针定。⑤ 运行时枚举（dump Express router stack）没跑，264 的静态分母与真实枚举的差集未知。

---

## 七个必须回答的问题

### #1 切法 · AI 代拍：甲（逐路由接管 + 未实现反代），待人复核

| 面 | 方案 | 依据 |
|---|---|---|
| HTTP | Rust 维护一张**路由归属表**（method + path pattern → `rust` / `ts`）。命中 `rust` 由 Rust 应答；其余逐字节反代给上游 TS：method / path / query / header（去掉逐跳头）/ body 流原样去，status / header / body 流原样回 | `index.js:540-648` 的挂载顺序即优先级；`/api/projects/files` 先于 `/api/projects`（`index.js:545-547, 589`） |
| 归属表生成规则 | **按注册顺序 + 挂载穿透**：同一路径被先注册的层遮蔽（`/api/projects/files` :547 先于 `/api/projects` :589；`/api/sessions/model` :637-639 先于 `/api/sessions` :640；`/api/mcp/config/read` `mcp.js:413` 被 `/api/mcp/config/:scope` `mcp.js:36` 遮蔽）。每条双注册 / 穿透路径各配 1 条探针用例，断言实际命中的 handler | 上述行号；`route-inventory.md` §9 |
| 分母 | **运行时枚举**（dump Express router stack + 加载期 hook 记录注册顺序与 mount chain）与静态清单对账，以枚举为准 | 工厂 / 条件 / 循环 / 未挂载见 `route-inventory.md` §9-10 |
| 处理侧标记 | Rust 对自持路由加响应头 `X-PilotDeck-Handler: rust`，反代路径标 `ts`；比对前剥离该头、覆盖检查读取该头，**证明请求确由 Rust 处理** | 新增机制，堵「反代全绿」假通过 |
| 迁移组 | 单条路由不够：按共享资源分组（#7）同组同批；G-EVT（进程内广播）在 `/ws` 接管前由跨进程广播桥承接 | `routes/memory.js:79`；`auth.db` 多写者；`routes/config.js:561` |
| WS | 三个端点**整条连接**反代：Rust 作为 WS 客户端连上游，帧双向搬运并 tee 到日志，close 双向传播 | 连接级内存态 `index.js:2364, 2388, 2337-2341`；样板 `index.js:2250-2288` |
| SSE | 走 HTTP 反代，但必须**不缓冲**（流式 body 直通、保留 `text/event-stream` 与 `X-Accel-Buffering: no`） | `index.js:1043-1046`；`routes/projects.js:542-545`；`routes/uploads.js:152-155` |
| 静态 | 先反代；Rust 接管后由 Rust 服务 `ui/dist` 与 `/memory-dashboard` | `index.js:881-889`；`index.js:861-878` |
| 回退 | 路由表条目翻回 `ts` 即回退；TS 进程在全量接管前一直存活 | 设计目标；无基线依据（新增机制） |
| 退役 | 全部路由对拍通过后才停 TS 上游 | 同 |

### #2 38 处进程内 `src/` · AI 代拍：三桶处置，待人复核

处置口径：**A = Rust 照协议直接实现**（纯类型/常量/解析/映射）；**B = 改走阶段 A 的 Rust gateway 类型化协议**；**C = 随宿主路由继续留守 TS，直到该路由被接管**。全部 38 条（本 worktree 逐行 grep，`路径:行号` 为 import 行）：

| # | 位置 | `src/` 模块（符号） | 处置 | 理由 |
|---|---|---|---|---|
| 1 | `ui/server/pilotdeck-bridge.js:60` | `src/gateway/index.js`（`createRemoteGateway`） | **B** | 这就是 gateway 客户端本身；Rust 用阶段 A 的协议客户端替换 |
| 2 | `ui/server/services/modelFreeHistory.js:1` | `src/web/server/listProjects.js` | **B** | 对应 gateway method `list_projects`（exp002 (a) 类） |
| 3 | `ui/server/services/modelFreeHistory.js:2` | `src/web/server/readSessionMessages.js` | **B** | 对应 `read_session_messages` |
| 4 | `ui/server/services/modelFreeHistory.js:3` | `src/session/index.js`（`listProjectSessions`） | **B** | 对应 `list_sessions` |
| 5 | `ui/server/projects.js:33` | `src/web/server/legacySessionPresentation.js` | **A** | 纯展示映射，无 IO |
| 6 | `ui/server/routes/uploads.js:8` | `src/gateway/dialog/UploadStore.js` | **B / C** | `upload_*` 在 exp002 里属「待定」；阶段 A 未提供前随上传路由留守 TS |
| 7 | `ui/server/services/uploadLimits.js:1` | `src/gateway/dialog/UploadStore.js`（`DEFAULT_UPLOAD_LIMITS`） | **A** | 只是常量，读 env 覆盖 |
| 8 | `ui/server/routes/commands.js:15` | `src/adapters/channel/protocol/ChannelCommandRegistry.js` | **A / B** | 内建命令注册表可移植；`commands_list` 是 gateway method |
| 9 | `ui/server/routes/commands.js:16` | `src/cli/commands/chatSearch.js` | **C**（待 A 确认） | 会话搜索；阶段 A method 清单冻结后再定 |
| 10 | `ui/server/routes/config.js:1` | `src/pilot/config/webSearchProviders.js` | **A** | 纯 provider 常量/判定 |
| 11 | `ui/server/routes/config.js:2` | `src/tool/builtin/additionalWebSearch.js` | **A** | 请求/结果映射 |
| 12 | `ui/server/routes/config.js:34` | `src/model/providerEndpoint.js` | **A** | endpoint 拼装 |
| 13 | `ui/server/routes/config.js:35` | `src/model/catalog/index.js` | **A** | provider 目录 |
| 14 | `ui/server/routes/config.js:36` | `src/network/fetch.js` | **A** | Rust HTTP 客户端 + 同样的错误分类 |
| 15 | `ui/server/routes/config.js:37` | `src/model/catalog/lookup.js` | **A** | 目录查找 |
| 16 | `ui/server/routes/projects.js:6` | `src/pilot/staffdeck/projectBinding.js` | **A** | 项目绑定（本地文件 + 远端），读同一份盘 |
| 17 | `ui/server/projects.js:24` | `src/pilot/staffdeck/projectBinding.js` | **A** | 同上 |
| 18 | `ui/server/routes/modules.js:16` | `src/pilot/staffdeck/projectBinding.js` | **A** | 同上 |
| 19 | `ui/server/projects.js:40` | `src/cron/protocol/types.js`（`mapCronRunOutcome`） | **A** | 纯映射 |
| 20 | `ui/server/pilotdeck-bridge.js:46` | `src/cli/proxy.js`（`installGlobalProxy`） | **A** | 出口代理；Rust 用 reqwest/hyper 的 proxy 配置 |
| 21 | `ui/server/pilotdeck-bridge.js:66` | `src/status/agentStatus.js` | **A** | 纯状态映射 |
| 22 | `ui/server/pilotdeck-host-capability-gateway.mjs:2` | `src/composition/publicHostWire.js` | **A** | 线格式编解码 |
| 23 | `ui/server/pilotdeck-host-remote-transport.mjs:1` | `src/composition/publicHostWire.js` | **A** | 同上 |
| 24 | `ui/server/discovery-plans.js:24` | `src/always-on/web/DiscoveryPlanService.js` | **A** | Always-on 本地运行时 |
| 25 | `ui/server/discovery-plans.js:25` | `src/always-on/web/DiscoveryPlanContext.js` | **A** | 同上 |
| 26 | `ui/server/discovery-plans.js:29` | `src/always-on/workspace/WorkspaceApply.js` | **A** | 同上 |
| 27 | `ui/server/discovery-plans.js:30` | `src/always-on/storage/AlwaysOnPaths.js` | **A** | 路径解析（读同一 `PILOT_HOME`） |
| 28 | `ui/server/discovery-plans.js:31` | `src/always-on/storage/DiscoveryStateStore.js` | **A** | 状态落盘 |
| 29 | `ui/server/services/always-on-run-history.js:7` | `src/always-on/web/AlwaysOnRunHistoryService.js` | **A** | 运行历史 |
| 30 | `ui/server/services/pilotdeckConfig.js:1` | `src/model/thinking/settings.js` | **A** | 配置解析 |
| 31 | `ui/server/services/pilotdeckConfig.js:8` | `src/pilot/config/parseGatewayConfig.js` | **A** | 配置解析 |
| 32 | `ui/server/services/pilotdeckConfig.js:9` | `src/pilot/config/parseToolsConfig.js` | **A** | 配置解析 |
| 33 | `ui/server/services/pilotdeckConfig.js:10` | `src/model/catalog/index.js` | **A** | provider 目录 |
| 34 | `ui/server/services/pilotdeckConfig.js:14` | `src/model/config/providerCredentialScope.js` | **A** | 凭据作用域 |
| 35 | `ui/server/services/modelConfigurationState.js:2` | `src/model/config/parseModelConfig.js` | **A** | 配置解析 |
| 36 | `ui/server/services/modelConfigurationState.js:3` | `src/pilot/config/loadPilotConfig.js` | **A** | 读同一 `pilotdeck.yaml` |
| 37 | `ui/server/services/modelConnectionProbe.js:4` | `src/model/providerEndpoint.js` | **A** | 同 #12 |
| 38 | `ui/server/services/modelConnectionProbe.js:5` | `src/network/fetch.js` | **A** | 同 #14 |

> 结论：**没有一条需要「强制改走 gateway 协议」**——4 条（#1-4）本来就是 gateway 通道，其余 34 条要么是纯逻辑（Rust 直接实现），要么随宿主路由留守 TS。阶段 B 结束时 TS `src/` 仍不能删的两条尾巴：`src/gateway/*` 客户端代码（阶段 A 的 crate 替代）与桌面旁路 `apps/desktop/src/main.ts:941`。
> 口径声明：上表 38 条只统计 `ui/server` 非测试文件里**直接** import `src/` 的行（本 worktree grep，共 16 个文件）；`services/desktopUpdates.spec.ts:6` import 的是 `apps/desktop`，不计入。

### #3 对拍装置 · AI 代拍：录放反代 + Playwright 场景 + 本地 mock 模型 + 阳性对照，待人复核

| 子问题 | 方案 | 依据 |
|---|---|---|
| 浏览器 API 层怎么录 | 在浏览器与 Rust/TS 之间放一个**录制反代**（Rust 或 Node 实现均可）：把 (method, path, query, 去掉 token 值的 header, body) 与 (status, header, body / 流分块) 原样落 jsonl；同一段 Playwright 脚本对「TS 直连」与「Rust 前置」各跑一遍，窄归一化后 diff | 无基线依据（新装置）；归一化思路照 `findings-prime-agent.md:14` |
| 分母怎么定 | **运行时枚举**：隔离 env 下加载真 `ui/server`，① dump Express router stack（`app._router.stack` 递归，工厂路由取 `handle.stack`、挂载前缀由 `layer.regexp` 还原），② 加载期 hook 包 `Router.prototype.use/.route` 与 METHOD，记录注册顺序与 mount chain；输出 JSON 与 `route-inventory.md` 对账（只静态 / 只运行时 / 前后缀不一致），**以枚举为分母** | 工厂（`modules.js:51`、`models.js:22`、`update.js:110`）、条件（`modules.js:58`）、循环（`index.js:669`）、未挂载/遮蔽（`update.js:112`、`mcp.js:413`） |
| 处理侧怎么证明 | Rust 对自持路由加响应头 `X-PilotDeck-Handler: rust`，反代路径标 `ts`；录制层记录该头，**覆盖检查**读它证明请求确由 Rust 应答；比对器在归一化时剥离它，避免污染金样本。`/ws` 的升级响应头同理 | 新增机制；堵「Rust 还没接管、反代全绿」的假通过 |
| 覆盖率怎么当硬门 | 冻结「有效路由集（枚举）× 用例」映射：**每条由 Rust 接管的路由 ≥1 条直接 HTTP 用例（直连 Rust 端口，断言 status/body + 处理侧头）或一条显式豁免记录**（`{route, reason, approver}`，如原生对话框 `/api/browse-filesystem/native-folder`、需 LibreOffice 的预览）。`check_coverage.py` 若任一 Rust 路由缺用例且无豁免 → FAIL | 新增规约；两家评审 P1「6 场景证明不了 263 条」 |
| 无类型 WS 消息怎么比 | 按连接录全帧（方向 + **录放层同一连接单调帧序 `seq`** + 原文）。顺序优先：① **同一连接内全序帧序列**（含 `type` 交错）；② 每种 `type` 的归一化 JSON 结构（键路径 + 值类型）与确定字段值。**录放层 `seq` 是排序键，保留并断言，绝不归一化**；应用负载里的 `createdAt`/`entryId` 才归一化。连接级时序（watch / 断线 / 替补 / 重连）单独建场景 | `index.js:2379-2635` 的 23 种 type；`index.js:2337-2341, 2645-2651` 连接生命周期 |
| SSE 怎么比 | 事件序列（event/data 归一化）为主；**硬门是「完整事件序列」+「到达延迟」**：① 事件不丢 / 不并 / 不乱序（逐条 event/data 比对）；② 每个事件的到达延迟 ≤ `k × 上游间隔 + m`（k/m 在未见 Rust 结果时冻结）。录制层记的每次 `read()` byte offset + 到达时刻**仅作诊断元数据**，**不得把 `read()` 分块数/分块边界当硬门**——传输层（TCP/TLS/Node stream）自然合并分块不等于语义聚合，会用假红盖掉真问题。补 `Last-Event-ID` 续传场景（`modules.js:336` 的 `job_events` / `preview_job_events`） | `index.js:1043-1046`；`routes/modules.js:336-339`；`docs/plan/001` 的 m/k 定阈值规矩 |
| PTY 怎么比 | `/shell` 不只比握手与 resize：`init` 后用 `input` 灌一条**固定命令输出哨兵**，但**哨兵完整字节不得出现在注入的输入文本里**——PTY 会回显输入，`index.js:2950-2953` 又把 PTY 输出原样发回，若输入写死 `printf 'PDSENTINEL_<nonce>'`，回显就能满足「哨兵出现」，命令没执行也绿。改用 shell 拼接：输入 `printf 'PDSENTINEL_%s\n' '<nonce>'`，期望输出 `PDSENTINEL_<nonce>`（整串不在输入中，回显无论怎么分帧都产生不了它），断言它出现在 PTY 输出帧；再验 `exit`（会话结束 / close 传播）与**重连**（`terminalSessions` 复用语义） | `index.js:2683, 2950, 2981, 2994`；`index.js:2664` |
| `ui/e2e` 的 `page.route` fixture 能不能复用 | **能，但只当前端回归门**：11 个文件用 `page.route` 伪造 `/api`、根本不碰后端（本 worktree 统计），切到 Rust 入口后重跑它们即可证明「前端没改、渲染不变」；**不能**当后端一致性对拍 | `findings-q3-q6-contracts.md:14`；本 worktree `grep page.route ui/e2e` 命中 11 个文件 |
| `ui/e2e/real` 要真模型怎么办 | 把 `PILOTDECK_E2E_LLM_CENTER_URL` / `PILOTDECK_E2E_API_KEY` 指向**本地 mock 模型服务**（OpenAI / Anthropic / Google 兼容 SSE），照 prime-agent 的 `mock_provider.py` 做法；`launch.mjs` 已支持三协议 URL，且已用高位端口 + 临时 `PILOT_HOME` / `DATABASE_PATH` | `ui/e2e/real/launch.mjs:17-29, 11-14, 78, 83-84`；`findings-prime-agent.md:20` |
| 阳性对照 | 注入 HTTP 头差异 / **交换同一连接的两帧顺序** / 删一帧 WS / **对 SSE 丢一个事件 / 把两个事件并成一个 / 把整条流攒到结束才发（到达延迟超上界）** / 截断一份 SSE / 改一处落盘，比对器必须判 FAIL；装置自比 k 遍一致才算可信 | `docs/plan/001` S3 的四项对照结构 |
| 规约 | 另立 `specs/ui-server-parity/{acceptance,normalization,accepted-diffs}.md`，人批后冻结；`acceptance.md` 必须写「路由 × 用例覆盖矩阵」为硬门，`normalization.md` 必须写 WS `seq` 全序与 SSE 完整事件 / 到达延迟口径；**不动** `specs/parity/` | `CLAUDE.md` §五「尺子不许自己改」 |

### #4 鉴权与 `auth.db` 兼容 · AI 代拍：Rust 自实现、读同一库、写者协调，待人复核

| 项 | 基线 | Rust 侧 |
|---|---|---|
| 库位置 | `DATABASE_PATH`（缺省 `ui/server/database/auth.db`） | 读同一 env / 默认路径 |
| JWT secret | `app_config.jwt_secret`，缺失时 `crypto.randomBytes(64).toString('hex')` 并写回 | `rusqlite`/`sqlx` 读同一 key；**接管前**绝不抢先写（避免与 TS 竞争） |
| JWT 语义 | HS256，claims `{userId, username}`，`expiresIn 7d`；校验后再查 `users`；`exp > iat` 且过半衰期时回 `X-Refreshed-Token` | `jsonwebtoken` crate 等价实现；头名逐字一致（CORS 暴露头 `index.js:512`） |
| 口令 | bcrypt 12 轮；单用户注册用事务占位 | `bcrypt` crate；注册 / 登录语义逐条对齐 |
| 默认关鉴权 | `IS_PLATFORM` 或 `PILOTDECK_DISABLE_LOCAL_AUTH !== '0'` 时跳过 JWT、取首个用户 | 同 env、同分支 |
| WS 鉴权 | `verifyClient` 从 `?token=` 或 `Authorization` 取；失败拒绝连接 | 反代期 Rust 只需**透传** query/header；接管 `/ws` 后自实现 |
| 其他表 | `api_keys` / `user_credentials` / `session_names` / `vapid_keys` / `push_subscriptions`（`init.sql`） | 相关路由（settings / user / sessions rename）接管时一并实现 |
| 迁移 | 无统一框架，只 `ALTER TABLE` 兜底 | 阶段 B **不改 schema**；写者由 #7 的共享资源组保证（G-AUTHDB 同组同批，或指定单一写者、另一侧经协议代理） |

证据：`database/db.js:26, 589-593`；`middleware/auth.js`（`jwt.verify` / `generateToken` / auto-refresh / `authenticateWebSocket`）；`routes/auth.js:2, 40-140`；`constants/config.js:8-16`；`index.js:478, 491-504`；`database/init.sql:5-16, 95-99`。

### #5 SSE · AI 代拍：流式透传、逐路由接管，待人复核

必须保真的 SSE 端点：`GET /api/search/conversations`（`index.js:1033`，`text/event-stream` 在 `:1043`，推 `result/progress/done/error`）、`GET /api/projects/clone-progress`（`routes/projects.js:542`，推 `data:` JSON）、`GET /api/uploads/:uploadId/events`（`routes/uploads.js:137`，头在 `:152`，推 `upload_started/progress/completed/failed`）、`POST /api/agent`（`routes/agent.js:910, 1184`，`stream=true` 时 POST 升级为 event-stream）。另有 NDJSON 流：`POST /api/update/apply`（`routes/webUpdate.js:12`）与模块 host 能力透传（`routes/modules.js:100-110`）。反代层不得改写 framing、不得丢/并事件、不得把整条流攒到结束；接管后由 Rust 写出与基线**语义**等价的流（事件完整、到达延迟有界）——**不要求 byte 级分块边界一致**。参考 `X-Accel-Buffering: no`（`index.js:1046`）。

**比对口径（写进 `specs/ui-server-parity/normalization.md`，B2 冻结）**：① **完整事件序列为主**（`event`/`data` 归一化后逐条比，不丢 / 不并 / 不乱序）；② **到达延迟上界为硬门**——每个事件的到达延迟 ≤ `k × 上游间隔 + m`（k/m 在未见 Rust 结果时冻结，`CLAUDE.md` §五「定阈值时不许看 Rust 运行结果」）；③ 录制层仍记每次 `read()` 的 byte offset + 到达时刻，但**只作诊断元数据**，**禁止**用 `read()` 分块数 / 分块边界做硬门或阳性对照——传输层自然合并分块不是语义聚合，会把正确实现判红；④ 保留 `X-Accel-Buffering: no` 等头。**续传**：续传头只在 GET 流分支读取（`routes/modules.js:334-339`，`streamOnly`）——`GET /api/modules/staffdeck-sdk/events?operation=job_events` 带 `Last-Event-ID` / `?lastEventId=`，`preview_job_events` 带 `?afterSeq=`；`POST /api/modules/staffdeck-sdk/call`（`routes/modules.js:449`）不读这些头，别用错分支。单列一条场景，断言从指定 id/seq 之后续传、不重放、不丢。

### #6 进程编排 · AI 代拍：先装置自编排、后改 supervisor / 桌面端，待人复核

| 入口 | 基线 | 阶段 B 目标态 |
|---|---|---|
| supervisor | `getRuntimeCommands` 起 `node --import tsx ui/server/index.js`（`webRuntimeSupervisor.js:31-38`）、gateway（`:50-57`）；gateway 端口就绪探测 `:102` | supervisor 起 Rust（3001）+ TS `ui/server`（私有端口）——**改这处需要一个 issue**，列在计划 003 Issue B6 |
| 桌面端 | `serverCommand()` 起 TS `ui/server`（`apps/desktop/src/main.ts:260-267`）；另有 `require('.../pilotdeckConfig.js')` 旁路（`:941`） | 同 supervisor；旁路单独登记（`gap.md` 1.4） |
| 开发 vite | `/api` `/ws` `/shell` `/memory-dashboard` → 3001（`ui/vite.config.js:131-144`） | 指向 Rust 后不变 |
| 实验期 | —— | **一律不改入口**：装置自己拉起进程，端口全高位，`PILOT_HOME` / `DATABASE_PATH` 指向 `_tmp/`；绝不碰用户正占的 3001 / 18789 / `~/.pilotdeck`（`CLAUDE.md` §五硬红线） |

### #7 迁移分组与写者协调 · AI 代拍：按共享资源分组、同组同批；G-EVT 走跨进程广播桥，待人复核

**为什么不是「单条路由单写者」。** 基线里多条路由共享同一份资源，按路由切会让 Rust 与 TS 同时写：

| 组 | 共享资源 | 写者（证据） | 迁移约束 |
|---|---|---|---|
| G-YAML | `pilotdeck.yaml`（同一文件） | 本进程写者：`routes/config.js:518, 747, 916, 979, 1517`；`routes/memory.js:79`；`routes/gateway.js:113`（`mutateConfigAndReload`）；`routes/onboarding.js:480`。**`index.js:3724` 不是写者**——它是外部编辑 watcher 收到磁盘变更后的 `process.emit('pilotdeck:config-broadcast')`（只读 + 发 G-EVT），真正的写由外部进程（vim/Cursor 等）完成 | 同组同批；写本身触发 G-EVT |
| G-AUTHDB | `auth.db`（同一 SQLite） | `routes/auth.js`（register / login last-login）；`routes/settings.js:2` 的 `apiKeysDb`/`credentialsDb`/`notificationPreferencesDb`/`pushSubscriptionsDb`；`routes/user.js:2` 的 `userDb`；`index.js:963, 995` 的 `sessionNamesDb` | 同一时刻只允许一侧写；接管任一写者即接管全组，或指定单一写者、另一侧经协议代理 |
| G-EVT | 进程内事件 `pilotdeck:config-broadcast` + `connectedClients` 广播 | 发出：`routes/config.js:561`、`routes/gateway.js:124`、`index.js:3724`（外部编辑 watcher 的 emit，非文件写者）；监听：`index.js:307` → `broadcastConfigReloaded`（`index.js:298-306`，**全员**）；直接广播：`index.js:966`（`session-deleted`，**按用户过滤** `index.js:964-968`）、`routes/taskmaster.js:1025, 1133, 1210, 1263, 1363` | 反代搬不动进程内事件；写路由要么与 `/ws` 同组迁移，要么经跨进程广播桥；桥须按 type/收件人分发，见下 |
| G-PLUGINS | `plugins.json` + 插件目录 | `routes/plugins.js:115, 152, 300` | 组内同批 |
| G-SKILLS | `~/.pilotdeck/skills` 或项目 skills | `routes/skills.js:446-459, 585, 602`；`commands.js` 的 `/skill_install` | 组内同批 |
| G-TASKDIR | 项目 `.taskmaster/` | `routes/taskmaster.js:802, 945, 1004, 1108, 1189, 1342, 1903` | 组内同批；写后广播归 G-EVT |
| G-GIT | 项目 `.git` | `routes/git.js` 写路由（:539, 578, 614, 695, 717, 744, 1145, 1190, 1258, 1343, 1414-1420, 1472-1475） | 组内同批 |
| G-MEMDB | 记忆服务 `control.sqlite`（**≠ `auth.db`**） | `routes/memory.js:5` 的 memoryService | 组内同批 |
| G-PERMS | `<PILOT_HOME>/permissions.json` | `routes/settings.js:25`（写）；`index.js:653`（读） | 组内同批 |
| G-UPLOAD | 上传目录 + `UploadStore` | `routes/uploads.js:106, 94, 195`；`src/gateway/dialog/UploadStore.ts` | 随宿主路由；SSE 独立 |
| G-SESS | transcript `projects/<id>/chats/*.jsonl` | `index.js:954, 1004`；`routes/messages.js:100`（fork） | 组内同批；删除后广播归 G-EVT |

**规则**：一条路由只有在以下之一成立时才能翻 `rust`——(a) 与它同组的所有路由**同批**翻；(b) 共享资源由已验证的跨进程契约（广播桥 / 单写者代理）中介。

**跨进程广播桥（过渡期，G-EVT 的解）**：TS `ui/server` 暴露内部端点（如 `POST /__internal/broadcast`），**`HOST=127.0.0.1` 绑定**（`index.js:3584` 默认 `0.0.0.0`；「仅回环」靠绑定落实，启动断言 `lsof`/`ss` 无 `0.0.0.0`/`*` 监听）**加** `<PILOT_HOME>/server-token`（ui 侧 `pilotdeck-bridge.js:79-81`，gateway 侧 `src/gateway/server/authToken.ts:14`）或派生密钥鉴权，body = 与基线同形的广播 payload 且带 `type`。**TS 收到后必须按事件类型分发，不能一律走全员广播**：`config:reloaded` 走 `broadcastConfigReloaded`（`index.js:298-306`，全员 + `publishConfigurationState`）；`session-deleted` 走基线的**按用户过滤**循环（`index.js:964-968`，仅 `client.__pilotdeckUserId === userId` 且 `readyState===OPEN` 才发，body 须带 `userId`）——若偷懒复用全员广播（`index.js:298` 的 `connectedClients.forEach`），A 用户删的会话会推给 B 用户，破坏基线语义；其余（满额删除的同用户 all-socket 广播 `index.js:2462-2466`、taskmaster 系列）按各自 `type` 与收件人语义归类后实现。Rust 接管写路由后**提交成功才调用**；调用失败的重试/超时、以及文件 watcher 二次广播的抑制（基线 `suppressNextWatchEvent`，`routes/gateway.js:115`）必须由探针实测后写进规约（`gap.md` §三「广播桥与文件 watcher 实验」）。`/ws` 被 Rust 接管后删桥，Rust 自持 `connectedClients`。桥只服务过渡期、默认关（env 开关），不改生产路径语义。

**迁移顺序（AI 代拍）**：B4 先迁**不与 G-EVT 耦合**的写组（G-GIT / G-PLUGINS / G-SKILLS；G-TASKDIR 读先迁、写待桥）；G-YAML 与 G-SESS 的删除广播在桥通过验证后再迁；`/ws`（B5）接管后桥退役。

## ⚠️ AI 代拍表（待人复核）

| # | 需要拍的 | 选项 | 决定与理由 |
|---|---|---|---|
| 1 | 切换粒度 | 整体替换 · Rust 前置逐路由接管 · 只做对拍装置不替换 | **AI 代拍：Rust 前置逐路由接管（待人复核）**。264 条路由 + 连接级 WS 态不可能一次替换；回退成本最低 |
| 2 | WS 怎么切 | 按 message type 拆 · 整条连接反代 · 自实现全套 | **AI 代拍：整条连接反代（待人复核）**。`index.js:2364/2388/2337-2341` 的连接级状态决定不能拆 |
| 3 | 38 处 `src/` 处置 | 全部改走 gateway · 全部 Rust 重实现 · 分桶 | **AI 代拍：分桶（待人复核）**。4 条 gateway 通道、其余纯逻辑或随路由留守；见 #2 表 |
| 4 | 对拍装置 | 复用 `ui/server/*.test.*` · 从零搭黑盒录放 · 只靠 `ui/e2e` | **AI 代拍：从零搭黑盒录放 + 复用 `ui/e2e` 作前端回归门（待人复核）**。前者是单路由内存 express、后者多数不碰后端 |
| 5 | 鉴权 | 反代给 TS · Rust 自实现读同一 `auth.db` · 换一套新鉴权 | **AI 代拍：Rust 自实现、读同一库、写者协调（待人复核）**。换新鉴权会改前端语义，触红线；`auth.db` 是 G-AUTHDB 组（#7），Rust 接管其中任一写路由即整组接管或单一写者代理 |
| 6 | 进程编排 | 现在就改 supervisor / 桌面端 · 装置自编排、后续 issue 再改 | **AI 代拍：装置自编排、后续 issue 再改（待人复核）**。与阶段 A 决策 #4 一致，实验期不碰基线入口 |
| 7 | 阶段门 | B 全部等 A 达标 · B 的实验/装置与 A/C 并行、产品代码等 A 达标 | **AI 代拍：后者（待人复核）**。探针与装置只读基线、只写本仓 `scripts` / `docs` / `specs`，与 A/C 产物零耦合；**B 的 Rust 产品 crate 严格等阶段 A 对拍达标**（`.42cog/intent.md` 红线收窄：「前一阶段对拍达标之前不进下一阶段」）。并行期全量对拍仍串行（`CLAUDE.md` §五） |
| 8 | 迁移粒度 / 写者 | 单条路由单写者 · 按共享资源分组同组同批 · 全库一把锁 | **AI 代拍：按共享资源分组、同组同批（待人复核）**。`routes/memory.js:79` 与 `/api/config` 同写 `pilotdeck.yaml`、`auth.db` 被多组共享，「单路由单写者」不成立；G-EVT 由过渡期广播桥承接，`/ws` 接管后删桥；见 #7 |
| 9 | 分母与覆盖率 | 静态清单计数 · 运行时枚举为准 · 不设覆盖门 | **AI 代拍：运行时枚举为准 + 路由 × 用例覆盖矩阵硬门（待人复核）**。工厂 / 条件 / 循环 / 未挂载使静态计数不可复算；6 场景盖不住 263 条，遗漏即 FAIL；见 #3 |

## 两把尺子

**复用分级**

| 来源 | 权限 | 这次用到哪一步 |
|---|---|---|
| PilotDeck 基线（AGPL，被移植对象） | 逻辑可移植，衍生作品同为 AGPL | 读懂 3001 协议与行为；HTTP/WS/SSE 实测输出作为对拍基准 |
| prime-agent（MIT） | 只读借鉴，读懂再自己写 | 借**做法**（mock 模型 HTTP 服务、按 key 归一化），不搬代码 |
| 阶段 C 的装置与规约（本仓） | 自有 | 借「装置先于代码、阳性对照、冻结规约」的方法论；`specs/parity/` 冻结文件**不动** |

**满意与停止线**：七个问题各有决定与行号依据、38 处逐条有处置、共享资源组与写者协调写清、探针验收口径写进计划 003——**到此为止**。运行时行为不再读源码猜，交给探针与录放装置去量。
