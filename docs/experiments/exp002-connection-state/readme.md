# exp002 · 18789 连接上的连接级状态：能不能按 method 切

> 2026-10-14。被查 `resources/PilotDeck` @ `cc7c4e6`。做法是读源码盘点（子 agent），主会话抽查 4 条（✔）。行号相对 `resources/PilotDeck/`。

## 假设

(a) 无状态只读的 method 能按 method 切给 Rust；(b) 会话域的只能按「会话整体归属某一侧」切；(c) 依赖连接状态的要求 Rust 自己持有连接。

## 做法

盘点 `src/gateway/server/GatewayWsConnection.ts` 及其直接持有对象里随连接存活的状态，追每类 `event` / `notification` 的触发与投递，把约 100 个 method 分三类，再看 `ui/server` 侧怎么开连接、怎么重连。

## 结论：三条假设都成立，(c) 比预想更硬

**连接级状态**

| 状态 | 证据 | 跨侧会怎样 |
|---|---|---|
| `interactionBinding {connectionId, generation}`，generation 是进程内全局计数 | `GatewayWsConnection.ts:8-13, 35-38`；经 `hello_ok` 下发（:116）；`submit_turn` `permission_decide` `elicitation_respond` `reconnect_interaction` 在服务端注入（:135, 504, 479, 460）；**权威记录在 TS 进程内存**（`InteractionReconnect.ts:69-70`） | Rust 自己生成 binding，TS 会判 `stale_binding`（`InProcessGateway.ts:775-789`） |
| `inFlightSessions` · `interactionSessions` | :126-127, 155, 456 | — |
| 断线清理 ✔ | :48-63：对两个集合里的会话 `disconnectInteraction`，在飞且无待决交互的会 `abortTurn` | Rust 不同步关上游，TS 就收不到断线，或者误 abort |
| 帧 `seq` ✔ | :129，**每条 `submit_turn` 自己的局部变量**，没有连接级计数 | 确定，不是噪声 |
| 待决的权限 / elicitation | 不在连接上，在 TS 的 PermissionBus / ElicitationBus，consume 时校验 binding（`GatewayPermissionBus.ts:102`） | Rust 处理 `permission_decide` 时拿不到 TS 的 bus |

**服务端主动下发**：`event` 只在 `submit_turn` 的流里、带原请求 id；`notification` 有两种——单连接（`permission_changed`）和全连接广播 ✔（`GatewayServer.ts:80-84`：`config_changed`、`user_dialog_changed`、`worktree_*`、`always-on:turn-event`）。Rust 处理写操作（`update_settings`、`reload_*`）时，TS 不会 markDirty，也不会广播 `config_changed`。

**连接与会话**：一条连接对多个会话，每个会话同一时刻只归一条连接。`ui/server` 只开**一条共享长连接**（`pilotdeck-bridge.js:568-570, 244`）；`createRemoteGateway` 本身不重连，重连在 `ui/server/services/gatewayConnectionCache.js` ✔（退避 1–30 秒）；重连后用旧 binding 发 `reconnect_interaction`，再用 `active_turn_snapshot` 重放（bridge :250, 463-553）。

**18789 上还挂着 HTTP**：`/api/web/*`、`/auth/local-token`、`/feishu/webhook`、`/api/module-host/*`、静态资源（`GatewayServer.ts:96-131`），代理要一并接。

**method 三类**（名单见子 agent 原报告，节选）：
- (a) 无状态只读（约 35 个）：`describe_server` `supported_agents` `list_projects` `describe_project` `model_catalog_list` `commands_list` `list_sessions` `read_session_messages` `usage_snapshot` …
- (b) 会话域（约 45 个）：`steer_turn` `abort_turn` `new/resume/close/fork_session` `session_model_*` `set_permission_mode` `sop_*` `always_on_*` …
- (c) 依赖连接状态（5 个）：`hello` `submit_turn` `reconnect_interaction` `permission_decide` `elicitation_respond`
- 待定（改全局状态或 TS 内存注册表）：`update_settings` `reload_*` `set_mcp_*` `skill_write/create/delete` `cron` 写操作 `upload_*` …

## 改变了哪个取舍

**取舍卡第 5 项「渐进粒度」有了答案**：
- 每条客户端连接 **1:1 映射**一条 TS 上游连接，`hello` / `hello_ok` 原样透传、沿用 TS 发的 binding，客户端断开时同步关上游；
- (a) 类可以**按 method** 由 Rust 本地应答；
- (b)(c) 类在「轮次与交互」整体迁到 Rust 之前**只能整块透传**——**不能按 method 切**；
- 写全局状态的那批，迁之前也得透传，否则 `config_changed` 广播会丢。

⇒ 收敛方向里「按 method 计数」的指标**只对 (a) 类成立**。(b)(c) 的进度要按「会话域 / 轮次」整块计。

## 结论的边界

- 只读源码，**没有跑**；运行时行为待 exp003（真 `ui/server` 连代理）证实。
- (a) 类「只读」指不改连接与会话运行时状态，**仍然读磁盘**（例如 `read_session_messages` 读 transcript），所以依赖落盘格式。
- `hello_ok` 里的 `serverInfo` 来自 `describeServer`（:115），Rust 接管 `describe_server` 时两边必须一致。
