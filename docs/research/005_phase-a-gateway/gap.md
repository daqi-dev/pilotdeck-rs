# 差距表 · 阶段 A：用 Rust 替换 18789 gateway

> 2026-10-15。被查 `resources/PilotDeck` @ `cc7c4e6`，本 worktree 主会话逐条回源核对（未经核对的行号一律不写）。
> 行号相对 `resources/PilotDeck/`。分级：P0 最高；P0/P1 清零才开工，P2 酌情。
> 上一轮（调研 002 的 `gap.md`）回答「边界选哪条」；本表只回答「阶段 A 的三条命门还差什么」。

## 一、基线对三条命门

### 命门 1 · `ui/server` 一行不改就能连上

**状态：基本具备，但从未实证；HTTP 面比预想宽。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 1.1 | 端口与地址全部可由环境变量重定向，无需改代码：gateway 监听 `PILOTDECK_GATEWAY_PORT`（缺省 18789）、`PILOTDECK_GATEWAY_HOST`、token `PILOTDECK_GATEWAY_TOKEN`/`PILOTDECK_TOKEN`（`src/cli/pilotdeck.ts:212-223`）；`ui/server` 连 `PILOTDECK_GATEWAY_URL`（缺省 `ws://127.0.0.1:18789/ws`）、token 文件 `PILOTDECK_GATEWAY_TOKEN_PATH`（缺省 `$PILOT_HOME/server-token`）（`ui/server/pilotdeck-bridge.js:77-81`） | 上述行号 | **没有实证**。exp002 只读源码；「真的 `ui/server`（带重连/重放的真客户端）连上一个 Rust 中间层」从未跑过 | exp003（`docs/plan/002` Issue A1）：Rust 透明代理站在高位端口，真 `ui/server` 用 env 指过去 | **P0** |
| 1.2 | `ui/server` 是 18789 的**唯一 WS 客户端**：单条共享长连接，惰性建立（首次调用才连），带退避重连（基础 1 秒、上限 30 秒）、重连后用旧 binding 发 `reconnect_interaction` 并以 `active_turn_snapshot` 重放 | `pilotdeck-bridge.js:244-265, 567-569`；`ui/server/services/gatewayConnectionCache.js:10-21`（退避默认值）；重连重放见 exp002 转引 `pilotdeck-bridge.js:250, 463-553` | Rust gateway 必须完整承受这条客户端的重连行为，而不是只服务「测试用的干净连接」 | exp003 场景里显式包含「杀掉代理→客户端退避重连→交互重放」 | **P0** |
| 1.3 | `ui/server` 可以不经 supervisor 单独启动：非监督模式下 gateway 状态为 `unmanaged`，不阻塞任何路由 | `ui/server/services/runtimeCoordination.js:12-14`；监听端口 `SERVER_PORT`（缺省 3001，占用时自动改随机端口至多 5 次）`ui/server/index.js:3583, 3588, 3598-3615` | 对拍装置与人肉验收都要用「单独起 `ui/server` + env 指向」这条路，得先确认它的启动依赖（如 `assertRequiredPilotDeckEnv`）在隔离 `PILOT_HOME` 下可满足 | exp003 第一步就是单独起 `ui/server` | P1 |
| 1.4 | 18789 上还有一整面 HTTP：`/health`、`/auth/local-token`（仅 loopback 返回 token）、`/feishu/webhook`、`/api/web/*`、`/api/module-host/describe|call`、`/api/module-host/approvals/status|resume`、`ui/dist` 静态资源（含 index.html 兜底） | `src/gateway/server/GatewayServer.ts:96-131`；`src/composition/publicHostHttpTransport.ts:11-12`；`src/composition/publicApprovalHttpTransport.ts:6-7`；`src/gateway/server/staticAssets.ts:13-28` | Rust 只做 WS 代理不够：`ui/server` 的 host 能力直接打 `GATEWAY_URL` 换算出的 http origin、带 Bearer token（`ui/server/pilotdeck-host-remote-transport.mjs:5-12`）；流式响应（ndjson/SSE）也要透 | Rust gateway 的 HTTP 面：先全量反向代理，`/health` 可本地应答 | **P0** |
| 1.5 | 服务端 WS 是**手写 RFC 6455**：accept 计算（sha1+GUID）、客户端帧必须掩码、只处理 text/close/ping 三种 opcode（ping 回 pong）、**不处理 continuation 分片**、服务端从不发 ping；`/ws` 之外的 upgrade 直接断 socket；鉴权失败以 close code 4001/4002/4003 关闭（`hello_required` / `protocol_mismatch` / `auth_failed` / `invalid_json` / `invalid_frame`） | `src/gateway/server/websocket.ts:6-8, 31-50, 52-75, 107-148`；upgrade 判定 `GatewayServer.ts:144-152`；close code `src/gateway/server/GatewayWsConnection.ts:79, 97, 101, 105` | Rust 实现要么复刻这些怪癖（不做分片重组、不发服务端 ping、同样的 close code），要么比它更健壮但不能**要求**客户端改变行为。浏览器与 Node undici 的 WebSocket 默认不发分片，但大帧（长 transcript、`read_session_messages` 结果）会走到 64 位长度分支 | 以对拍为准：帧头行为、close code 语义逐条对齐；不预设「标准库默认就对」 | **P0** |
| 1.6 | 进程编排三处入口：supervisor 起 `pilotdeck.ts server`（`ui/server/webRuntimeSupervisor.js:50-57`）并对 `PILOTDECK_GATEWAY_PORT` 做端口就绪探测（`:102, 214`）；桌面端 `findFreePort(18789)` 后把 `PILOTDECK_GATEWAY_PORT`/`PILOTDECK_GATEWAY_URL` 注入两个子进程（`apps/desktop/src/main.ts:138, 153-177`），且**总是自己 spawn TS gateway**（`:250-259`） | 上述行号 | supervisor 的 env 是继承的（`webRuntimeSupervisor.js:103-109`），所以「Rust 占 18789 + TS 挪到 `PILOTDECK_GATEWAY_PORT` 指定端口」不需要改任何代码；但桌面端会继续用自己的 TS gateway——阶段 A 内桌面端**用不上 Rust**，人肉验收走 Web UI | 阶段 A 不改入口（红线）；桌面端接入留给阶段 B 或人批（见 `decision.md` 第 4 行） | P1 |

### 命门 2 · 「一致」能由机器判

**状态：观测点齐全，装置要从零搭。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 2.1 | 帧协议是**有类型**的：6 种帧（hello / hello_ok / request / response / event / notification）、101 个 method 枚举（`frames.ts:21-122` 逐行数得 101）、载荷类型集中在 `types.ts`（1666 行，但 import 自约 20 个模块，`types.ts:1-72`）；协议版本常量 `"1.2"`（`src/gateway/protocol/version.ts:1`） | 上述行号 | 类型只存在于 TS 编译期，**没有运行时 schema**（module protocol v2 有现成 JSON Schema，网关边界没有——对照 `tests/protocol/module-protocol-contract.spec.ts:19-55` 静态核对的是那份，不是帧协议的） | 从 TS 类型导出 JSON Schema 冻结为对拍基准（`decision.md` 第 3 行）；基线已有 `ajv`（`package.json:61-62`），TS 侧校验不用加依赖 | **P0** |
| 2.2 | WS 客户端形态现成：`GatewayWsClient` 只吃 `url + token`（hello 超时 10 秒，`:118-125`；`request`/`stream` 两种调用，`:152-166`；结构化错误保留 `code/validation/details`，`:207-216`），从 `src/gateway/index.ts:112-119` 一处导出；真端口测试样板现成（`tests/gateway/bridge-interaction-reconnect.spec.ts:125-141`：`startGatewayServer({port: 0, token})` + `GatewayWsClient` + `RemoteGateway`），全仓共 10 个测试文件这么打真 WS | 上述行号 | 这些测试都在**进程内**构造 gateway 或直接起 TS server；没有「同一脚本打两个不同实现、落盘、比对」的装置 | 阶段 A 对拍装置（`decision.md` 第 2 行）：TS 驱动 + 帧级录制 jsonl + 比对器 + 归一化 + 阳性对照 | **P0** |
| 2.3 | 三个观测点都物理可隔离：`PILOT_HOME` 可整体替换（`src/pilot/paths.ts:9, 21`，调研 002 已核）；token 文件也在其下（`authToken.ts:12-15`）；run 注册表落 `$PILOT_HOME/runs/registry.json`（`src/cli/createLocalGateway.ts:996`） | 上述行号 | 落盘比对要面对 jsonl/sqlite/md 混合格式与无统一迁移（调研 002 findings-q3q4 已记）；网关侧新增的 `runs/registry.json` 与上传产物目录也要进比对清单 | 对拍装置的「落盘快照」步骤 + 归一化规则（新规约，不动已冻结的 `specs/parity/`） | **P0** |
| 2.4 | 非确定性字段清单已知：帧 `seq` 是每条 `submit_turn` 的局部计数、确定不抹（`GatewayWsConnection.ts:129`）；事件 `sequence`/`createdAt`、run seq、各种 UUID 属噪声（exp002 与调研 002 findings-q6 已盘点） | `GatewayWsConnection.ts:129`；其余见 exp002 | 阶段 C 的归一化规约（`specs/parity/normalization.md`）**已冻结、不许改**，且它面向语义事件 trace，不面向帧日志与 HTTP 响应 | 阶段 A 自己的规约草稿（帧/落盘/HTTP 三类各自的归一化与 ID 一致映射原则），人批后冻结 | **P0** |
| 2.5 | 服务端主动下发有三类：`event`（只在 `submit_turn` 流里、带原请求 id）；单连接 `notification`（`permission_changed`，在流内触发与 `permission_decide` 后触发两处）；全连接广播（`config_changed`、`user_dialog_changed`、`worktree_*`、`always-on:turn-event`，经 `broadcastNotification` 遍历所有连接） | `GatewayWsConnection.ts:144-151, 176-186`；广播实现 `GatewayServer.ts:80-84`；`config_changed` 源 `src/cli/GatewayRuntimeRefreshBundle.ts:102, 144, 159-160`；`user_dialog_changed` `createLocalGateway.ts:1057`；`worktree_*`/`always-on:turn-event` `pilotdeck.ts:77-86` | 对拍必须把「服务端主动推送」也录进来并比对**时序**（同一连接上 event 与 notification 的交错顺序），只比请求-响应不够 | 帧级录制按连接记全量帧 + 单调序号；比对含顺序断言 | **P0** |

### 命门 3 · 渐进切换时连接级状态不出错

**状态：exp002 已盘点清楚；Rust 侧的对应机制要从零建。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 3.1 | `interactionBinding {connectionId, generation}`：connectionId 是每连接随机 UUID，generation 是**进程内全局计数**（模块级可变变量自增）；经 `hello_ok` 下发；`submit_turn`/`permission_decide`/`elicitation_respond`/`reconnect_interaction` 在服务端注入连接自己的 binding；权威记录（含 `lastBindings`）在 TS 进程内存 | `GatewayWsConnection.ts:8-13, 35-38, 110-118, 135, 456-461, 479, 504`；`src/interaction/InteractionReconnect.ts:66-71`（`pending`/`bindings`/`lastBindings` 三个内存 Map） | 透传期 Rust **不得**自造 binding：`hello`/`hello_ok` 原样透传、沿用 TS 下发的 binding；generation 是 TS 进程计数，Rust 侧没有任何等价物也不需要 | 代理架构从第一天就按「每客户端连接 1:1 一条上游连接」设计（exp002 的结论，`decision.md` 第 1 行确认） | **P0** |
| 3.2 | 断线清理：连接关闭时对 `inFlightSessions ∪ interactionSessions` 逐个 `disconnectInteraction`，在飞且无待决交互的会 `abortTurn`；这是 TS 侧唯一的断线联动 | `GatewayWsConnection.ts:48-63` | 客户端断开时 Rust 必须**同步关上游**：不关 → TS 收不到断线（会话泄漏、误保持交互）；提前关或重连后关错连接 → TS 误 abort | 代理的双向关闭语义 + 断线/重连场景进对拍必测集 | **P0** |
| 3.3 | 待决权限/elicitation 不在连接上，在进程级 bus；`permission_decide` consume 时校验 binding 是否当前（不当前则静默丢弃返回 undefined） | `src/gateway/permission/GatewayPermissionBus.ts:96-103`（consume 的 binding 校验在 `:102`）；sdk 控制连接的豁免逻辑 `GatewayWsConnection.ts:492-506` | Rust 若本地应答 `permission_decide`（它属于 (c) 类），拿不到 TS 的 bus；且 `clientName === "sdk"` 的豁免分支依赖连接上的登记——透传是最安全的 | (c) 类 5 个 method（`hello` `submit_turn` `reconnect_interaction` `permission_decide` `elicitation_respond`）整条透传，且代理保持 `hello.clientName` 原样 | **P0** |
| 3.4 | (a) 类约 35 个只读 method 的「只读」仍会读盘（如 `read_session_messages` 读 transcript）；`describe_server` 的能力清单来自注入的运行时 options，`hello_ok.serverInfo` 与它是同一来源 | exp002（分类名单）；`GatewayWsConnection.ts:115`（hello_ok 的 serverInfo = `describeServer()`）、`:361-362`（describe_server 转发同一调用）；能力拼装 `src/gateway/client/InProcessGateway.ts:2088-2114`；fallback 实现 `pilotdeck.ts:584` | Rust 本地应答 `describe_server` 时，返回的 `capabilities` 若与 TS 上游实际能力不一致，客户端能力探测会走错分支（可选 method 缺能力时 TS 返回 `CAPABILITY_UNAVAILABLE`，`GatewayWsConnection.ts:261-263` 等） | 过渡期 `describe_server` 保持透传；本地应答的 (a) 类逐个迁时，`serverInfo` 一致性进对拍断言 | **P0** |
| 3.5 | 写全局状态的 method（`update_settings`、`reload_*`、`set_mcp_*`、skill 写、cron 写、upload）在 TS 侧会 `markAllDirty` 并广播 `config_changed`；Rust 本地处理会导致 TS 不失效缓存、广播丢失 | `GatewayRuntimeRefreshBundle.ts:144, 159-160`（markAllDirty + 广播） | exp002 已定：这批在对应状态迁走前一律透传 | 无需新机制：透传清单里显式列出 | P1 |

## 二、候选之间 —— Rust 前置的两种姿势

| | 甲：纯透明代理（一切透传） | 乙：代理 + (a) 类本地应答 |
|---|---|---|
| 改动面 | 只有连接搬运 | 加本地实现（读同一 `PILOT_HOME`） |
| 风险面 | 低：帧不改写 | `describe_server`/能力清单一致性（3.4）；读盘格式的语义对齐 |
| 能证明什么 | 命门 1 连接路径 + 装置本身（对拍应全绿） | (a) 类逐个迁移的可行性 |
| 定位 | **第一步**（exp003 / Issue A1） | **第二步**（Issue A4），且 `describe_server` 暂不本地化 |

## 三、该不该自己跑 —— 要跑的实验

| 实验 | 为什么必须跑 | 会改变哪个取舍 |
|---|---|---|
| **exp003**：真 `ui/server` 连站在别的端口的透明代理 | 命门 1 的「放行」条件；exp002 全部结论建立 在读源码上，运行时行为（重连、重放、HTTP 面）没验证过 | 若走不通，代理架构与「前端不改一行」都要重想 |
| **exp004**（已在计划 001 排给阶段 C）：尺子复核 | 阶段门：C 达标才进 A 的代码迁移 | 决定 A4 何时开工 |
