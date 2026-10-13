# 决策点 · 阶段 A：用 Rust 替换 18789 gateway

> 2026-10-15。**这一份是给 AI 用的裁刀，不是给你答的题。**
> 阶段 A 的边界已经在调研 002 与 exp002 里定过一轮（先 C 再 A 再 B；A = 18789 帧协议）。本目录只回答「阶段 A 自己怎么落地」。
> 行号均相对 `resources/PilotDeck/`（基线 @ `cc7c4e6`，本 worktree 已回源核对）。

## 我要作的决定

Rust gateway 以什么形态站上 18789、按什么粒度从 TS 手里接 method、拿什么装置证明「接过去之后行为没变」。

## 一、这件事上，我不可妥协的是哪几条

| # | 不可妥协的 | 为什么它是命门 | 怎么算满足 |
|---|---|---|---|
| 1 | **原版 `ui/server`（浏览器前端经 3001）一行不改就能连上**。允许的只有环境变量重定向，且重定向钩子在基线里本来就有（`ui/server/pilotdeck-bridge.js:77-81` 的 `PILOTDECK_GATEWAY_URL` / `PILOTDECK_GATEWAY_TOKEN_PATH`；`src/cli/pilotdeck.ts:212,220,223` 的 `PILOTDECK_GATEWAY_PORT` / `PILOTDECK_GATEWAY_TOKEN`）。**桌面端不在阶段 A 验收范围**：桌面壳总是自己 spawn TS gateway（`apps/desktop/src/main.ts:250-258`）并覆盖连接地址（`:162-163`），阶段 A 不改 `apps/desktop` 就不可能让桌面用上 Rust；是否提前纳入待人拍板（`decision.md` 拍板表，不代拍），Web UI 验收通过不得宣称桌面兼容 | 收敛方向本身（`.42cog/intent.md`「前端一行不改」）。改了 `ui/` 就分不清差异来自前端改动还是后端重写；桌面端纳入验收等于改桌面启动逻辑，越阶段 B 的界 | 真的 `ui/server` 进程连上站在**高位端口**的 Rust gateway：`hello` → `hello_ok` → method 调用 → 事件流 → 通知，全按基线协议走通；断线重连（`gatewayConnectionCache.js` 退避 1–30 秒）与 `reconnect_interaction` 重放也走通；桌面端明确标注「未验收」 |
| 2 | **对拍能机器判**。帧与事件流、落盘副作用、HTTP 响应三个观测点，同一份 TS 驱动脚本对 TS 侧与 Rust 侧各跑一遍，归一化后逐字节可比；**帧日志一律在 driver 侧录制**（驱动进程自己落盘），不靠被测代理自己 tee——被测侧不能自证；「本地应答未转发上游」由装置自己的上游观测点（插在 Rust gateway 与 TS 上游之间、属 `scripts/gateway-parity` 的组件）录帧证明 | 判不了就只能凭感觉。阶段 C 已经证明「先有可信的尺子再动代码」是唯一可行的顺序（`docs/plan/001`）；18789 上还挂着 HTTP 路由与静态资源，只比 WS 帧不够；纯透传也能让「输出一致」拿到 100%，指标必须绑定「Rust 本地应答」这一事实 | 比对器 + 归一化规则 + **阳性失败对照**（故意注入差异必须判 FAIL，证明「一致」不是恒真）三者齐备并冻结；任何登记为「已接受差异」的条目有类别与依据；迁移指标只计「Rust 本地应答且对拍一致」的 method（分母 = `method-inventory.md` 冻结的 (a) 类 15 个），计分者须同时有上游观测点「未转发」的录帧证据 |
| 3 | **渐进切换时连接级状态不出错**。任何时刻，一条客户端连接 1:1 映射一条 TS 上游连接；`interactionBinding` 的权威始终在「持有该会话交互运行时」的那一侧（透传期 = TS）；断线双向同步关闭 | exp002 的核心结论：binding 权威在 TS 进程内存（`GatewayWsConnection.ts:35-38`；`InteractionReconnect.ts:69-71`）；断线时 TS 要对在飞会话做 `disconnectInteraction` + 可能 `abortTurn`（`GatewayWsConnection.ts:48-63`）。Rust 自作主张生成 binding 会被判 `stale_binding`，漏关上游会让 TS 误 abort 或永不清理 | 代理态全场景帧级对拍一致（含断线、重连、重放场景）；(a) 类本地应答不改变 `hello_ok` 透传语义（`serverInfo` 仍来自 TS 的 `describeServer()`，`GatewayWsConnection.ts:115`）；Rust 本地应答 `describe_server` 时其结果与 `hello_ok.serverInfo` 的能力清单一致 |

## 二、知道了它们，我要作的是哪一个决定

> **Rust gateway 先以什么姿势站在 18789 前面（纯代理 / 代理 + 只读本地应答）、对拍装置搭在哪、协议类型从哪来、进程怎么编排。** 见 `decision.md`。

## 三、哪些看起来相关，但知道了也不改变这个决定

- `ui/server` 内部约 5 万行的实现（23 组 `/api` 路由、25 种无类型 WS 消息）——阶段 B 的事；阶段 A 只把它当**唯一客户端**对待
- `ui/server` 进程内 import `src/` 的 38 处与对 `auth.db`、记忆库的直读——那是「TS 的 `src/` 什么时候能删」的问题（阶段 B 之后），不影响 18789 边界怎么切
- 各 method 的业务语义（SOP、Always-on、cron 的内部逻辑）——按块迁移时再读
- 性能、并发吞吐——个人学习项目，正确性优先
- 参考仓里与「WS 网关 / 代理 / 帧对拍 / 类型导出」无关的功能（codex 的沙箱、云同步等）

## 四、我的预判（已核实）

| 我怀疑 | 核实结果 |
|---|---|
| 端口写死 18789，改端口要动代码 | **不成立**：三个进程都认环境变量——gateway 监听 `PILOTDECK_GATEWAY_PORT`（`src/cli/pilotdeck.ts:212,220`）、`ui/server` 连 `PILOTDECK_GATEWAY_URL`（`pilotdeck-bridge.js:77-78`）、supervisor 原样继承 env（`webRuntimeSupervisor.js:103-109`）并用同一变量做就绪探测（`:102,214`） |
| 桌面端写死起 TS gateway，Rust 占 18789 就撞车 | **部分成立但比预想松**：桌面端自己 `findFreePort(18789)` 挑端口、再把 `PILOTDECK_GATEWAY_PORT`/`URL` 注入两个子进程（`apps/desktop/src/main.ts:138,162-163`）——不会撞车，但桌面端**永远用自己的 TS gateway**（`:250-258`）。**阶段 A 不验收桌面端**（与 `decision.md` 统一，待人拍板），人肉验收走 Web UI；Web 通过不得宣称桌面兼容 |
| gateway 的 WS 是标准库实现，代理随便转发 | **不成立**：服务端是**手写 RFC 6455**（`src/gateway/server/websocket.ts`）——客户端帧必须掩码（`:133-134`）、不处理分片/continuation 帧、服务端不发 ping 只回 pong（`:64-66`）、close 帧带自定义 code（4001/4002/4003）。Rust 侧必须按这些怪癖对齐 |
| `ui/server` 必须由 supervisor 拉起才能工作 | **不成立**：非监督模式下网关状态是 `unmanaged`（`ui/server/services/runtimeCoordination.js:12-14`），连接是惰性单例（`pilotdeck-bridge.js:244,567-569`）——可以单独 `node --import tsx ui/server/index.js` 起，配 env 即连任意 gateway |
| 18789 上只有 WS | **不成立**：还有 `/health`、`/auth/local-token`（仅 loopback）、`/feishu/webhook`、`/api/web/*`、`/api/module-host/{describe,call}`、`/api/module-host/approvals/*` 与 `ui/dist` 静态资源（`GatewayServer.ts:88-135`；`publicHostHttpTransport.ts:11-12`；`publicApprovalHttpTransport.ts:6-7`；`staticAssets.ts:13-28`）。`ui/server` 的 host 能力直接打 `GATEWAY_URL` 换算出的 http origin + Bearer token（`pilotdeck-host-remote-transport.mjs:5-12`） |

---

**放行**：三条命门都能被「有 / 部分 / 没有」回答、每条证据带行号，可以进 `gap.md`。
