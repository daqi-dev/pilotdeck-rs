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
| 1.5 | 服务端 WS 是**手写 RFC 6455**：accept 计算（sha1+GUID）、客户端帧必须掩码、只处理 text/close/ping 三种 opcode（ping 回 pong）、**不处理 continuation 分片**、服务端从不发 ping；`/ws` 之外的 upgrade 直接断 socket；鉴权失败以 close code 4001/4002/4003 关闭（`hello_required` / `protocol_mismatch` / `auth_failed` / `invalid_json` / `invalid_frame`） | `src/gateway/server/websocket.ts:6-8, 31-50, 52-75, 107-148`；upgrade 判定 `GatewayServer.ts:144-152`；close code `src/gateway/server/GatewayWsConnection.ts:79, 89, 97, 101, 105`（`:89` = 4002 `invalid_frame`：hello 之后的非 request 帧） | Rust 实现要么复刻这些怪癖（不做分片重组、不发服务端 ping、同样的 close code），要么比它更健壮但不能**要求**客户端改变行为。第 1 轮评审补齐的怪癖细目（DeepSeek 回源核对）：① **RSV 位不校验**——`readClientFrame` 只取 `first & 0x0f`（`websocket.ts:110`），RSV 非零帧当普通帧解；② **continuation（0x0）静默丢弃**——`handleData` 只认 0x8/0x9/0x1 三种 opcode，其余消耗掉不报错（`websocket.ts:60-72`）；③ **客户端 close 回 1000**——收到 0x8 时 `close()` 用缺省 code 1000，不回显对方 code（`websocket.ts:40, 62-64`）；④ **unmasked 帧直接 `throw` 冒泡**——不是 close 帧，连接以异常路径断（`websocket.ts:133-134`）。四条都要进 acceptor 复刻清单与 A2 对拍场景 | 以对拍为准：帧头行为、close code 语义逐条对齐；不预设「标准库默认就对」 | **P0** |
| 1.6 | 进程编排三处入口：supervisor 起 `pilotdeck.ts server`（`ui/server/webRuntimeSupervisor.js:50-57`）并对 `PILOTDECK_GATEWAY_PORT` 做端口就绪探测（`:102, 214`）；桌面端 `findFreePort(18789)` 后把 `PILOTDECK_GATEWAY_PORT`/`PILOTDECK_GATEWAY_URL` 注入两个子进程（`apps/desktop/src/main.ts:138, 153-177`），且**总是自己 spawn TS gateway**（`:250-258`），并覆盖连接地址（`:162-163`） | 上述行号 | supervisor 的 env 是继承的（`webRuntimeSupervisor.js:103-109`），所以「Rust 占高位端口 + TS 挪到 `PILOTDECK_GATEWAY_PORT` 指定端口」不需要改任何代码；但桌面端会继续用自己的 TS gateway——**阶段 A 不验收桌面端**（must-know 与 decision 已统一），接入与是否提前纳入验收待人拍板（`decision.md` 拍板表）；Web UI 验收通过不得宣称桌面兼容 | 阶段 A 不改入口（红线）；桌面端不验收、待人拍板 | P1 |

### 命门 2 · 「一致」能由机器判

**状态：观测点齐全，装置要从零搭。**

| # | 事实 | 证据 | 差距是什么 | 修起来要什么 | 分级 |
|---|---|---|---|---|---|
| 2.1 | 帧协议是**有类型**的：6 种帧（hello / hello_ok / request / response / event / notification）、101 个 method 枚举（`frames.ts:21-122` 逐行数得 101，已全部分类并**冻结**进 `method-inventory.md`：(a)15 / (b)46 / (c)4 / 待定 36）、载荷类型集中在 `types.ts`（1666 行，但 import 自约 20 个模块，`types.ts:1-72`）；协议版本常量 `"1.2"`（`src/gateway/protocol/version.ts:1`） | 上述行号 + `method-inventory.md` | ~~类型只存在于 TS 编译期，**没有运行时 schema**~~ **已关闭（2026-10-14，A3，AI 代批待人复核）**：JSON Schema 已从基线 `frames.ts` + `types.ts` 导出并冻结——`schema/gateway-protocol.schema.json`（根 `$ref WsGatewayFrame`，101 method 枚举 + Gateway 接口载荷类型）+ `schema/gateway-methods.snapshot.json`（方法快照）；断言 `scripts/gateway-parity/check_schema.py` + ajv 正负例 `scripts/gateway-parity/validate_fixtures.mjs` 进 CI（`check` job）；基线核对由 `gateway-schema-baseline` job 按冻结 commit `cc7c4e6` 浅克隆基线跑 `--check` 与 L1–L3（本地亦可，`PDRS_REQUIRE_BASELINE=1` 缺基线即 FAIL）；冻结登记与口径见 `specs/gateway-parity/schema-FROZEN.md`。原始缺口描述留档：类型只存在于 TS 编译期，没有运行时 schema（module protocol v2 有现成 JSON Schema，网关边界没有——对照 `tests/protocol/module-protocol-contract.spec.ts:19-55` 静态核对的是那份，不是帧协议的） | ~~从 TS 类型导出 JSON Schema 冻结为对拍基准~~ 已落地：A3 导出器 `scripts/gateway-parity/export_schema.mjs`（`--check` 验再生成一致）；静态断言含「枚举=101、与 `method-inventory.md` 集合与顺序一致、开放联合保持开放字符串、版本常量四处一致」；TS 侧校验用基线已有 `ajv`，Rust 侧 serde 对拍（schema × fixtures）A4 接手 | ~~P0~~ **关闭** |
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
| 3.3 | 待决权限/elicitation 不在连接上，在进程级 bus；`permission_decide` consume 时校验 binding 是否当前（不当前则静默丢弃返回 undefined） | `src/gateway/permission/GatewayPermissionBus.ts:96-103`（consume 的 binding 校验在 `:102`）；sdk 控制连接的豁免逻辑 `GatewayWsConnection.ts:492-506` | Rust 若本地应答 `permission_decide`（它属于 (c) 类），拿不到 TS 的 bus；且 `clientName === "sdk"` 的豁免分支依赖连接上的登记——透传是最安全的 | (c) 类 4 个 method（`submit_turn` `reconnect_interaction` `permission_decide` `elicitation_respond`，`method-inventory.md`；exp002 曾计 5 个是把帧类型 `hello` 误计入 method）整条透传，且代理保持 `hello.clientName` 原样 | **P0** |
| 3.4 | (a) 类**冻结为 15 个**只读 method（`method-inventory.md`，重审后自 exp002 的「约 35」收缩——凡读内存运行态的一律降级：`read_session_messages` 合并轮次快照并校验 epoch `InProcessGateway.ts:2850-2860`、`export_session_transcript` 查 `hasActiveTurn` `createLocalGateway.ts:1165-1167`）；`describe_server` 的能力清单来自注入的运行时 options，`hello_ok.serverInfo` 与它是同一来源 | `method-inventory.md`（每行带行号）；`GatewayWsConnection.ts:115`（hello_ok 的 serverInfo = `describeServer()`）、`:361-362`（describe_server 转发同一调用）；能力拼装 `src/gateway/client/InProcessGateway.ts:2088-2114`；fallback 实现 `pilotdeck.ts:584` | Rust 本地应答 `describe_server` 时，返回的 `capabilities` 若与 TS 上游实际能力不一致，客户端能力探测会走错分支（可选 method 缺能力时 TS 返回 `CAPABILITY_UNAVAILABLE`，`GatewayWsConnection.ts:261-263` 等） | 过渡期 `describe_server` 保持透传（不入 (a)）；本地应答的 (a) 类逐个迁时，`serverInfo` 一致性进对拍断言 | **P0** |
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

## 四、exp003 实测回填（2026-10-14，Issue #15 / A1）

详证：[exp003 readme](../../experiments/exp003-transparent-proxy/readme.md)。未重新调研或改动冻结 method 清单。

- **1.1 / 1.3 已实证**：原版 standalone ui/server 经 Rust 前置连通、hello_ok 原帧可见；项目/会话/历史与文本轮次通过。隔离还必须设置 `WORKSPACES_ROOT` 与 `DATABASE_PATH`，注册真实项目需 `.cwd` 标记。
- **1.2 保留 P0，修正“重连后会自动重放”的断言**：真实 bridge 能退避重连，但 `GatewayWsClient.ts:93-100` 在 disconnect handlers 前清掉 hello；`RemoteGateway.ts:111-112` 不再提供旧 binding，bridge `:255-258, :2127-2130` 无法保存退休 binding，实测未发 reconnect_interaction / active_turn_snapshot。不能按预期存在恢复流程就声称其运行成功。
- **3.1 / 3.2 补充边界证据**：驱动提前保存旧 binding，直连与代理都得到 `outcome=reconnected`、原 pending requestId 与 `active=true`。后续 elicitation answer 两侧均 delivered=false；待单独定因修复，不宣称轮次恢复完成。
- **1.4 已实证主要路由与流式搬运**：health、local-token、web 项目/文件读写、带 Bearer/principal 的 POST module-host/describe 均通过，未鉴权 401 保留。测试专用 SSE/NDJSON route 验证增量返回，未把它当实际 module-host 全操作覆盖；生产 UI 静态资源缺少构建，未验收。
- **1.5 补充标准库 close 的实际陷阱**：立即 drop 导致正常 close 1000→1006，仅 flush 自动回显导致空 close→1005。Rust 现取上游实际 close reply 写回；空/custom client close 均得到 TS 的 1000，4001/4002/4003 的 code/reason 原样返回。protocol_mismatch 的具体 code 是 **4001**。RSV/continuation/unmasked 原有怪癖未在 A1 闭合。
- **装置证据**：相同官方客户端场景各录 19 条，最终零差异；修改 hello_ok.serverVersion 的阳性对照得到 FAIL / exit 1 / 唯一帧差异。只有 A1 临时帧比较口径，A2 全量装置缺口仍未关闭。
- **环境限制**：macOS managed sandbox 原生 fs.watch 报 EMFILE，并可经 CLI uncaughtException handler 静默 exit 1。最终夹具明确用 preload 浅层 stat 轮询替代，未修改基线源码；递归扩展热加载不在验证结论内。
